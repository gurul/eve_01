//! Port of bridge/tests/test_rosbridge.py: the v2 protocol over the MemoryBus, one real server
//! per test on a free port. The bus is real; the client is tokio-tungstenite.

use std::io::Write;
use std::time::Duration;

use eve_memory_bus::MemoryBus;
use futures::{SinkExt, StreamExt};
use serde_json::json;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use super::*;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

fn free_port() -> i64 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port() as i64
}

fn msg(v: Value) -> Msg {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

fn bound_bus() -> MemoryBus {
    let bus = MemoryBus::new();
    bus.bind_loop(tokio::runtime::Handle::current());
    bus
}

async fn serve(bus: &MemoryBus) -> RosbridgeServer {
    serve_with(bus, DEFAULT_QUEUE_SIZE).await
}

async fn serve_with(bus: &MemoryBus, queue_size: usize) -> RosbridgeServer {
    let server = RosbridgeServer::new(bus.clone(), "127.0.0.1", free_port(), queue_size);
    server.start().await.unwrap();
    server
}

async fn connect(url: &str) -> Ws {
    tokio_tungstenite::connect_async(url).await.unwrap().0
}

/// The next text frame as JSON, or Err on timeout.
async fn recv_within(ws: &mut Ws, secs: f64) -> Result<Value, ()> {
    let fut = async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Text(t))) => return serde_json::from_str(t.as_str()).unwrap(),
                Some(Ok(_)) => continue,
                other => panic!("connection ended: {other:?}"),
            }
        }
    };
    tokio::time::timeout(Duration::from_secs_f64(secs), fut).await.map_err(|_| ())
}

async fn recv(ws: &mut Ws) -> Value {
    recv_within(ws, 2.0).await.expect("timed out waiting for a message")
}

async fn send(ws: &mut Ws, op: Value) {
    ws.send(Message::text(op.to_string())).await.unwrap();
}

async fn sleep(secs: f64) {
    tokio::time::sleep(Duration::from_secs_f64(secs)).await;
}

fn the_client(server: &RosbridgeServer) -> Arc<Client> {
    server.inner.clients.lock().values().next().cloned().expect("a client")
}

#[tokio::test]
async fn test_subscribe_then_bus_publish_reaches_the_client() {
    let bus = bound_bus();
    let server = serve(&bus).await;
    {
        let mut ws = connect(&server.url()).await;
        send(&mut ws, json!({"op": "subscribe", "id": "s1", "topic": "/eve/state", "type": "eve_msgs/AgentState"})).await;
        sleep(0.05).await;
        bus.publish("/eve/state", msg(json!({"state": "wake"})));
        assert_eq!(recv(&mut ws).await, json!({"op": "publish", "topic": "/eve/state", "msg": {"state": "wake"}}));
        assert!(server.client_count() == 1 && server.stats().sent == 1);
        server.stop().await;
    }
    assert_eq!(server.client_count(), 0);
}

#[tokio::test]
async fn test_prefix_subscription_receives_memory_topics_but_not_state() {
    let bus = bound_bus();
    let server = serve(&bus).await;
    let mut ws = connect(&server.url()).await;
    send(&mut ws, json!({"op": "subscribe", "id": "p", "topic": "/eve/memory/*"})).await;
    sleep(0.05).await;
    bus.publish("/eve/state", msg(json!({"state": "wake"})));
    bus.publish("/eve/memory/lesson", msg(json!({"action": "hint"})));
    bus.publish("/eve/memory/observation", msg(json!({"thought": "the mug moved"})));
    let (first, second) = (recv(&mut ws).await, recv(&mut ws).await);
    assert_eq!([&first["topic"], &second["topic"]], [&json!("/eve/memory/lesson"), &json!("/eve/memory/observation")]);
    assert!(recv_within(&mut ws, 0.2).await.is_err());
    server.stop().await;
}

#[tokio::test]
async fn test_unsubscribe_stops_delivery_and_disconnect_clears_the_bus() {
    let bus = bound_bus();
    let server = serve(&bus).await;
    {
        let mut ws = connect(&server.url()).await;
        send(&mut ws, json!({"op": "subscribe", "id": "a", "topic": "/eve/state"})).await;
        send(&mut ws, json!({"op": "subscribe", "id": "b", "topic": "/eve/memory/lesson"})).await;
        sleep(0.05).await;
        assert_eq!(bus.subscription_count(), 2);
        send(&mut ws, json!({"op": "unsubscribe", "id": "a", "topic": "/eve/state"})).await;
        sleep(0.05).await;
        bus.publish("/eve/state", msg(json!({"state": "wake"})));
        bus.publish("/eve/memory/lesson", msg(json!({"action": "step"})));
        assert_eq!(recv(&mut ws).await["topic"], "/eve/memory/lesson");
        assert!(recv_within(&mut ws, 0.2).await.is_err());
        ws.close(None).await.unwrap();
        // drain until the server's close reply ends the stream
        while let Ok(Some(_)) = tokio::time::timeout(Duration::from_secs(1), ws.next()).await {}
    }
    sleep(0.1).await;
    assert_eq!(bus.subscription_count(), 0);
    assert_eq!(server.client_count(), 0);
    server.stop().await;
}

#[tokio::test]
async fn test_inbound_publish_reaches_the_bus_only_for_known_topics() {
    let bus = bound_bus();
    let got: Arc<Mutex<Vec<(String, Msg)>>> = Arc::default();
    let g = got.clone();
    bus.subscribe("/eve/*", move |t, m| g.lock().push((t.to_string(), m.clone())));
    let server = serve(&bus).await;
    {
        let mut ws = connect(&server.url()).await;
        send(&mut ws, json!({"op": "publish", "topic": "/eve/memory/remember", "msg": {"text": "the mug is Sam's"}})).await;
        send(&mut ws, json!({"op": "publish", "id": "x", "topic": "/nope/topic", "msg": {"text": "no"}})).await;
        let status = recv(&mut ws).await;
        assert!(status["op"] == "status" && status["level"] == "error" && status["id"] == "x");
        assert_eq!(status["msg"], "cannot publish to unknown topic '/nope/topic'");
        send(&mut ws, json!({"op": "publish", "topic": "/eve/state", "msg": "not an object"})).await;
        let status = recv(&mut ws).await;
        assert_eq!(status["level"], "error");
        assert_eq!(status["msg"], "publish needs an object msg");
        sleep(0.05).await;
    }
    server.stop().await;
    assert_eq!(*got.lock(), vec![("/eve/memory/remember".to_string(), msg(json!({"text": "the mug is Sam's"})))]);
}

#[tokio::test]
async fn test_call_service_answers_with_service_response() {
    let bus = bound_bus();
    bus.register_service("/eve/memory/recall", |args: Msg| async move {
        Ok(msg(json!({"results": [{"title": args["query"]}]})))
    });
    bus.register_service("/eve/memory/broken", |_args: Msg| async move { Err(anyhow::anyhow!("worker down")) });
    let server = serve(&bus).await;
    let mut ws = connect(&server.url()).await;
    send(&mut ws, json!({"op": "call_service", "id": "c1", "service": "/eve/memory/recall", "args": {"query": "mug"}})).await;
    let reply = recv(&mut ws).await;
    assert_eq!(
        reply,
        json!({"op": "service_response", "service": "/eve/memory/recall", "id": "c1",
               "result": true, "values": {"results": [{"title": "mug"}]}})
    );
    send(&mut ws, json!({"op": "call_service", "id": "c2", "service": "/nowhere"})).await;
    let reply = recv(&mut ws).await;
    assert!(reply["result"] == false && reply["values"] == json!({"error": "no such service"}) && reply["id"] == "c2");
    send(&mut ws, json!({"op": "call_service", "id": "c3", "service": "/eve/memory/broken", "args": {}})).await;
    let reply = recv(&mut ws).await;
    assert!(reply["result"] == false && reply["values"]["error"].as_str().unwrap().contains("worker down"));
    server.stop().await;
}

#[tokio::test]
async fn test_throttle_rate_drops_events_that_come_too_fast() {
    let bus = bound_bus();
    let server = serve(&bus).await;
    let mut ws = connect(&server.url()).await;
    send(&mut ws, json!({"op": "subscribe", "id": "t", "topic": "/eve/state", "throttle_rate": 200})).await;
    sleep(0.05).await;
    for i in 0..5 {
        bus.publish("/eve/state", msg(json!({"i": i})));
    }
    let mut got = vec![recv(&mut ws).await];
    if let Ok(m) = recv_within(&mut ws, 0.3).await {
        got.push(m);
    }
    assert!((1..=2).contains(&got.len()) && got[0]["msg"] == json!({"i": 0}));
    server.stop().await;
}

#[tokio::test]
async fn test_a_client_that_never_reads_drops_its_own_messages_and_never_blocks_the_bus() {
    let bus = bound_bus();
    let server = serve_with(&bus, 3).await;
    let mut ws = connect(&server.url()).await;
    send(&mut ws, json!({"op": "subscribe", "id": "q", "topic": "/eve/state"})).await;
    sleep(0.05).await;
    // Stall the writer so the queue fills: the client is alive but not draining.
    let client = the_client(&server);
    let writer = client.writer.lock().take().unwrap();
    writer.abort();
    let _ = writer.await;
    let started = Instant::now();
    for i in 0..20 {
        bus.publish("/eve/state", msg(json!({"i": i})));
    }
    assert!(started.elapsed() < Duration::from_secs(1));
    let dropped = server.stats().dropped;
    assert!(dropped >= 17 && client.dropped.load(Ordering::SeqCst) == dropped);
    assert_eq!(client.queue.lock().len(), 3);
    let oldest: Value = serde_json::from_str(&client.queue.lock().pop_front().unwrap()).unwrap();
    assert_eq!(oldest["msg"], json!({"i": 17})); // the oldest went first
    server.stop().await;
}

#[tokio::test]
async fn test_malformed_json_gets_a_status_error_and_the_connection_stays_open() {
    let bus = bound_bus();
    let server = serve(&bus).await;
    let mut ws = connect(&server.url()).await;
    ws.send(Message::text("{not json")).await.unwrap();
    let status = recv(&mut ws).await;
    assert_eq!(status, json!({"op": "status", "level": "error", "msg": "message is not valid JSON"}));
    ws.send(Message::text("[1, 2]")).await.unwrap();
    let status = recv(&mut ws).await;
    assert_eq!(status, json!({"op": "status", "level": "error", "msg": "message must be a JSON object"}));
    send(&mut ws, json!({"op": "dance", "id": "d"})).await;
    let status = recv(&mut ws).await;
    assert!(status["level"] == "error" && status["msg"].as_str().unwrap().contains("dance") && status["id"] == "d");
    assert_eq!(status["msg"], "unsupported op: 'dance'");
    send(&mut ws, json!({"op": "subscribe", "id": "s", "topic": "/eve/state"})).await;
    sleep(0.05).await;
    bus.publish("/eve/state", msg(json!({"state": "wake"})));
    assert_eq!(recv(&mut ws).await["msg"], json!({"state": "wake"}));
    server.stop().await;
}

#[tokio::test]
async fn test_advertise_unadvertise_and_set_level_are_accepted_silently() {
    let bus = bound_bus();
    let server = serve(&bus).await;
    let mut ws = connect(&server.url()).await;
    send(&mut ws, json!({"op": "advertise", "id": "a", "topic": "/eve/memory/remember", "type": "eve_msgs/Remember"})).await;
    send(&mut ws, json!({"op": "set_level", "level": "error"})).await;
    sleep(0.05).await;
    assert!(the_client(&server).advertised.lock().contains("/eve/memory/remember"));
    send(&mut ws, json!({"op": "unadvertise", "id": "a", "topic": "/eve/memory/remember"})).await;
    assert!(recv_within(&mut ws, 0.2).await.is_err());
    assert!(the_client(&server).advertised.lock().is_empty());
    send(&mut ws, json!({"op": "subscribe", "id": "u", "topic": "/not/a/topic"})).await;
    let status = recv(&mut ws).await;
    assert!(status["level"] == "error" && status["msg"].as_str().unwrap().contains("subscribed anyway"));
    assert_eq!(status["msg"], "unknown topic /not/a/topic; subscribed anyway");
    server.stop().await;
}

const ROSLIBPY_CLIENT: &str = r#"
import sys, threading, json, roslibpy
port = int(sys.argv[1])
seen = threading.Event()
got = []
client = roslibpy.Ros(host="127.0.0.1", port=port)
client.run(timeout=5)
assert client.is_connected
topic = roslibpy.Topic(client, "/eve/state", "eve_msgs/AgentState")
topic.subscribe(lambda m: (got.append(m), seen.set()))
print("subscribed", flush=True)
ok = seen.wait(5)
print(json.dumps(got[0] if ok else None), flush=True)
topic.unsubscribe()
client.terminate()
"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: needs python3 with roslibpy installed (never a project dependency)"]
async fn test_the_reference_client_roslibpy_receives_a_message() {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let bus = bound_bus();
    let server = serve(&bus).await;
    let mut child = tokio::process::Command::new("python3")
        .args(["-c", ROSLIBPY_CLIENT, &server.port().to_string()])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let first = tokio::time::timeout(Duration::from_secs(10), lines.next_line()).await.unwrap().unwrap();
    assert_eq!(first.as_deref(), Some("subscribed"));
    sleep(0.3).await;
    bus.publish("/eve/state", msg(json!({"state": "wake"})));
    let second = tokio::time::timeout(Duration::from_secs(10), lines.next_line()).await.unwrap().unwrap().unwrap();
    let got: Value = serde_json::from_str(&second).unwrap();
    assert_eq!(got, json!({"state": "wake"}), "roslibpy never received the publish");
    let _ = child.wait().await;
    server.stop().await;
}

async fn connect_with_origins(url: &str, origins: &[&str]) -> Result<Ws, WsError> {
    let mut req = url.into_client_request().unwrap();
    for o in origins {
        req.headers_mut().append("Origin", HeaderValue::from_str(o).unwrap());
    }
    tokio_tungstenite::connect_async(req).await.map(|(ws, _)| ws)
}

#[tokio::test]
async fn test_a_browser_page_is_refused_and_a_desktop_client_is_not() {
    // v3 safety review: any web page the owner opens could reach ws://127.0.0.1 and read or write
    // eve's memory. A handshake with an Origin header (a browser page) is refused with 403; no
    // Origin (a desktop client) connects.
    let bus = bound_bus();
    let server = serve(&bus).await;
    for origin in ["https://evil.invalid", "http://127.0.0.1:8000", "null"] {
        match connect_with_origins(&server.url(), &[origin]).await {
            Err(WsError::Http(resp)) => {
                assert_eq!(resp.status().as_u16(), 403);
                let body = String::from_utf8(resp.body().clone().unwrap_or_default()).unwrap();
                assert_eq!(body, format!("Failed to open a WebSocket connection: invalid Origin header: {origin}.\n"));
                assert_eq!(resp.headers()["Content-Type"], "text/plain; charset=utf-8");
            }
            other => panic!("expected a 403, got {:?}", other.map(|_| "connected")),
        }
    }
    // the positive control: no Origin, connected
    let mut ws = connect(&server.url()).await;
    send(&mut ws, json!({"op": "subscribe", "id": "s1", "topic": "/eve/state", "type": "eve_msgs/AgentState"})).await;
    sleep(0.05).await;
    bus.publish("/eve/state", msg(json!({"state": "wake"})));
    assert_eq!(recv(&mut ws).await["msg"], json!({"state": "wake"}));
    assert_eq!(server.stats().clients_total, 1);
    server.stop().await;
}

// -- beyond the Python file --

#[tokio::test]
async fn an_empty_origin_is_refused_and_two_origins_are_a_bad_request() {
    let bus = bound_bus();
    let server = serve(&bus).await;
    match connect_with_origins(&server.url(), &[""]).await {
        Err(WsError::Http(resp)) => {
            assert_eq!(resp.status().as_u16(), 403);
            let body = String::from_utf8(resp.body().clone().unwrap_or_default()).unwrap();
            assert_eq!(body, "Failed to open a WebSocket connection: empty Origin header.\n");
        }
        other => panic!("expected a 403, got {:?}", other.map(|_| "connected")),
    }
    match connect_with_origins(&server.url(), &["https://a.invalid", "https://b.invalid"]).await {
        Err(WsError::Http(resp)) => {
            assert_eq!(resp.status().as_u16(), 400);
            let body = String::from_utf8(resp.body().clone().unwrap_or_default()).unwrap();
            assert_eq!(body, "Failed to open a WebSocket connection: invalid Origin header: multiple values.\n");
        }
        other => panic!("expected a 400, got {:?}", other.map(|_| "connected")),
    }
    assert_eq!(server.stats().clients_total, 0);
    server.stop().await;
}

#[tokio::test]
async fn outbound_frames_are_python_json_dumps() {
    let bus = bound_bus();
    let server = serve(&bus).await;
    let mut ws = connect(&server.url()).await;
    send(&mut ws, json!({"op": "subscribe", "topic": "/eve/state"})).await;
    sleep(0.05).await;
    bus.publish("/eve/state", msg(json!({"state": "wake", "n": 1.0, "w": "café"})));
    let raw = loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => break t.as_str().to_string(),
            Some(Ok(_)) => continue,
            other => panic!("{other:?}"),
        }
    };
    assert_eq!(raw, r#"{"op": "publish", "topic": "/eve/state", "msg": {"state": "wake", "n": 1.0, "w": "café"}}"#);
    server.stop().await;
}

#[tokio::test]
async fn ids_and_ops_are_shown_as_python_does() {
    let bus = bound_bus();
    let server = serve(&bus).await;
    let mut ws = connect(&server.url()).await;
    send(&mut ws, json!({"op": 5, "id": 7})).await;
    assert_eq!(recv(&mut ws).await, json!({"op": "status", "level": "error", "msg": "unsupported op: 5", "id": "7"}));
    send(&mut ws, json!({"id": true})).await;
    assert_eq!(recv(&mut ws).await, json!({"op": "status", "level": "error", "msg": "unsupported op: None", "id": "True"}));
    send(&mut ws, json!({"op": ["a", 1.5], "id": 2.5})).await;
    assert_eq!(recv(&mut ws).await, json!({"op": "status", "level": "error", "msg": "unsupported op: ['a', 1.5]"}));
    send(&mut ws, json!({"op": "publish", "msg": {}})).await;
    assert_eq!(recv(&mut ws).await["msg"], "cannot publish to unknown topic None");
    send(&mut ws, json!({"op": "subscribe", "id": "e"})).await;
    assert_eq!(recv(&mut ws).await, json!({"op": "status", "level": "error", "msg": "subscribe needs a topic", "id": "e"}));
    send(&mut ws, json!({"op": "call_service", "service": ""})).await;
    assert_eq!(recv(&mut ws).await["msg"], "call_service needs a service");
    // a binary frame is JSON too, as json.loads(bytes)
    ws.send(Message::binary(br#"{"op": "nope"}"#.to_vec())).await.unwrap();
    assert_eq!(recv(&mut ws).await["msg"], "unsupported op: 'nope'");
    server.stop().await;
}

#[tokio::test]
async fn a_known_prefix_subscription_gets_no_status_and_unsubscribe_without_id_removes_all() {
    let bus = bound_bus();
    let server = serve(&bus).await;
    let mut ws = connect(&server.url()).await;
    send(&mut ws, json!({"op": "subscribe", "id": "1", "topic": "/eve/*"})).await;
    send(&mut ws, json!({"op": "subscribe", "id": "2", "topic": "/eve/*"})).await;
    assert!(recv_within(&mut ws, 0.2).await.is_err());
    assert_eq!(bus.subscription_count(), 2);
    send(&mut ws, json!({"op": "unsubscribe", "id": "3", "topic": "/eve/*"})).await;
    sleep(0.05).await;
    assert_eq!(bus.subscription_count(), 2);
    send(&mut ws, json!({"op": "unsubscribe", "topic": "/eve/*"})).await;
    sleep(0.05).await;
    assert_eq!(bus.subscription_count(), 0);
    server.stop().await;
}

#[tokio::test]
async fn unadvertise_of_a_list_ends_the_connection_like_a_python_raise() {
    let bus = bound_bus();
    let server = serve(&bus).await;
    let mut ws = connect(&server.url()).await;
    send(&mut ws, json!({"op": "subscribe", "topic": "/eve/state"})).await;
    send(&mut ws, json!({"op": "unadvertise", "topic": ["x"]})).await;
    let mut closed = false;
    while let Ok(Some(item)) = tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
        if let Ok(Message::Close(frame)) = item {
            assert_eq!(frame.map(|f| u16::from(f.code)), Some(1000));
            closed = true;
        }
    }
    assert!(closed);
    assert_eq!(server.client_count(), 0);
    assert_eq!(bus.subscription_count(), 0);
    server.stop().await;
}

#[tokio::test]
async fn a_message_over_one_mebibyte_closes_with_1009() {
    let bus = bound_bus();
    let server = serve(&bus).await;
    let mut ws = connect(&server.url()).await;
    // Two frames, each under the limit, together over it: the server reads both, then refuses.
    use tokio_tungstenite::tungstenite::protocol::frame::coding::{Data, OpCode};
    use tokio_tungstenite::tungstenite::protocol::frame::Frame;
    let half = "x".repeat(MAX_SIZE / 2 + 10).into_bytes();
    ws.send(Message::Frame(Frame::message(half.clone(), OpCode::Data(Data::Text), false))).await.unwrap();
    ws.send(Message::Frame(Frame::message(half, OpCode::Data(Data::Continue), true))).await.unwrap();
    let mut close = None;
    while let Ok(Some(item)) = tokio::time::timeout(Duration::from_secs(2), ws.next()).await {
        if let Ok(Message::Close(Some(frame))) = item {
            close = Some((u16::from(frame.code), frame.reason.as_str().to_string()));
        }
    }
    let size = 2 * (MAX_SIZE / 2 + 10);
    assert_eq!(close, Some((1009, format!("over size limit ({size} > {MAX_SIZE} bytes)"))));
    sleep(0.05).await;
    assert_eq!(server.client_count(), 0);
    server.stop().await;
}

#[tokio::test]
async fn a_panicking_service_answers_with_an_error_and_keyerror_is_shown_like_python() {
    let bus = bound_bus();
    bus.register_service("/eve/memory/recall", |_args: Msg| async move {
        if true {
            panic!("boom");
        }
        Ok(Msg::new())
    });
    let server = serve(&bus).await;
    let mut ws = connect(&server.url()).await;
    send(&mut ws, json!({"op": "call_service", "service": "/eve/memory/recall", "args": "not a dict"})).await;
    let reply = recv(&mut ws).await;
    assert_eq!(reply, json!({"op": "service_response", "service": "/eve/memory/recall", "result": false,
                             "values": {"error": "Error: boom"}}));
    let e = ServiceError::NoSuchService("/x".into());
    assert_eq!(format!("{}: {}", error_kind(&e), error_text(&e)), "KeyError: 'no such service: /x'");
    let e = ServiceError::Failed(anyhow::Error::new(io::Error::other("refused")));
    assert_eq!(format!("{}: {}", error_kind(&e), error_text(&e)), "OSError: refused");
    server.stop().await;
}

#[tokio::test]
async fn closing_a_client_cancels_its_service_calls() {
    let bus = bound_bus();
    let finished = Arc::new(AtomicBool::new(false));
    let f = finished.clone();
    bus.register_service("/eve/memory/recall", move |_args: Msg| {
        let f = f.clone();
        async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            f.store(true, Ordering::SeqCst);
            Ok(Msg::new())
        }
    });
    let server = serve(&bus).await;
    let mut ws = connect(&server.url()).await;
    send(&mut ws, json!({"op": "call_service", "service": "/eve/memory/recall"})).await;
    sleep(0.05).await;
    let client = the_client(&server);
    assert_eq!(client.tasks.lock().len(), 1);
    server.stop().await;
    assert!(client.tasks.lock().is_empty());
    assert!(!finished.load(Ordering::SeqCst));
}

#[test]
fn an_unbounded_queue_never_drops_and_the_warning_is_logged_once() {
    let logs = global_logs();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let names = rt.block_on(async {
        let bus = bound_bus();
        let mut names = Vec::new();
        for (size, expect_dropped) in [(0usize, 0u64), (2, 8)] {
            let server = serve_with(&bus, size).await;
            let mut ws = connect(&server.url()).await;
            send(&mut ws, json!({"op": "subscribe", "topic": "/eve/state"})).await;
            sleep(0.05).await;
            let client = the_client(&server);
            names.push(client.name.clone());
            let writer = client.writer.lock().take().unwrap();
            writer.abort();
            let _ = writer.await;
            for i in 0..10 {
                bus.publish("/eve/state", msg(json!({"i": i})));
            }
            assert_eq!(server.stats().dropped, expect_dropped);
            assert_eq!(client.queue.lock().len() as u64, 10 - expect_dropped);
            server.stop().await;
        }
        names
    });
    let text = logs.text();
    let warning = |name: &str| format!("rosbridge: {name} is not keeping up; dropping its oldest messages");
    assert_eq!(text.matches(&warning(&names[0])).count(), 0);
    assert_eq!(text.matches(&warning(&names[1])).count(), 1);
    assert!(text.contains("rosbridge: serving eve's memory at ws://127.0.0.1:"));
    assert!(text.contains("server listening on ('127.0.0.1', "));
    assert!(text.contains("connection open"));
    assert!(text.contains("server closing") && text.contains("server closed"));
    assert!(text.contains("rosbridge: stopped (0 sent, 8 dropped, 1 clients over its life)"));
}

#[tokio::test]
async fn start_twice_is_a_no_op_port_zero_records_the_port_and_stop_twice_is_safe() {
    let bus = bound_bus();
    let server = RosbridgeServer::new(bus.clone(), "127.0.0.1", 0, DEFAULT_QUEUE_SIZE);
    assert_eq!(server.url(), "ws://127.0.0.1:0");
    server.start().await.unwrap();
    let port = server.port();
    assert_ne!(port, 0);
    server.start().await.unwrap();
    assert_eq!(server.port(), port);
    let _ws = connect(&server.url()).await;
    server.stop().await;
    server.stop().await;
    assert!(!server.is_serving());
    assert!(tokio_tungstenite::connect_async(server.url()).await.is_err());
    let bad = RosbridgeServer::new(bus, "127.0.0.1", 70000, DEFAULT_QUEUE_SIZE);
    assert_eq!(bad.start().await.unwrap_err().to_string(), "bind(): port must be 0-65535.");
    assert_eq!(Stats { sent: 1, dropped: 2, clients_total: 3 }.as_map().keys().cloned().collect::<Vec<_>>(),
               ["sent", "dropped", "clients_total"]);
}

#[tokio::test]
async fn a_busy_port_fails_to_start_with_an_os_error() {
    let bus = bound_bus();
    let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = taken.local_addr().unwrap().port() as i64;
    let server = RosbridgeServer::new(bus, "127.0.0.1", port, DEFAULT_QUEUE_SIZE);
    let err = server.start().await.unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
    assert!(!server.is_serving());
}

#[test]
fn throttle_uses_the_injected_clock() {
    // A deterministic clock: 0.0, then 0.1, 0.3, 0.35 seconds.
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let bus = bound_bus();
        let now = Arc::new(Mutex::new(0.0f64));
        let n = now.clone();
        let server = RosbridgeServer::with_clock(bus.clone(), "127.0.0.1", free_port(), 64, Arc::new(move || *n.lock()));
        server.start().await.unwrap();
        let mut ws = connect(&server.url()).await;
        send(&mut ws, json!({"op": "subscribe", "topic": "/eve/state", "throttle_rate": 200})).await;
        sleep(0.05).await;
        for (t, i) in [(0.0, 0), (0.1, 1), (0.3, 2), (0.35, 3)] {
            *now.lock() = t;
            bus.publish("/eve/state", msg(json!({"i": i})));
        }
        assert_eq!(recv(&mut ws).await["msg"], json!({"i": 0}));
        assert_eq!(recv(&mut ws).await["msg"], json!({"i": 2}));
        assert!(recv_within(&mut ws, 0.2).await.is_err());
        server.stop().await;
    });
}

#[test]
fn python_text_forms() {
    assert_eq!(py_float_repr(1.0), "1.0");
    assert_eq!(py_float_repr(0.1), "0.1");
    assert_eq!(py_float_repr(-2.5), "-2.5");
    assert_eq!(py_float_repr(1e16), "1e+16");
    assert_eq!(py_float_repr(1.5e16), "1.5e+16");
    assert_eq!(py_float_repr(123456789012345.6), "123456789012345.6");
    assert_eq!(py_float_repr(1e-5), "1e-05");
    assert_eq!(py_float_repr(0.0001), "0.0001");
    assert_eq!(py_float_repr(-0.0), "-0.0");
    assert_eq!(py_float_repr(100.0), "100.0");
    assert_eq!(py_json(&json!({"a": [1, 2.0, null, true], "b": {}, "c": [], "d": "x\"\n\u{1}é"})),
               r#"{"a": [1, 2.0, null, true], "b": {}, "c": [], "d": "x\"\n\u0001é"}"#);
    assert_eq!(py_repr(&json!({"k": [null, false, "it's"]})), r#"{'k': [None, False, "it's"]}"#);
    assert_eq!(py_str_repr("a\tb\u{a0}\u{7f}\\"), r"'a\tb\xa0\x7f\\'");
    assert_eq!(py_str_repr("both ' and \""), r#"'both \' and "'"#);
    assert_eq!(py_str_repr("\u{2028}é"), "'\\u2028é'");
    // The recorded differences: serde_json reads `-0` as a float and refuses a lone surrogate.
    assert_eq!(py_json(&serde_json::from_str("-0").unwrap()), "-0.0");
    assert!(serde_json::from_str::<Value>(r#""\ud800""#).is_err());
    // Format characters added in Unicode 14/15, an unassigned code point, and a printable
    // variation selector, exactly as Python 3.13 shows them.
    assert_eq!(py_str_repr("\u{890}\u{13430}\u{378}a\u{e0100}"), "'\\u0890\\U00013430\\u0378a\u{e0100}'");
}

#[derive(Clone, Default)]
struct LogBuf(Arc<Mutex<Vec<u8>>>);

impl Write for LogBuf {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl LogBuf {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }
}

/// Everything this test binary logs (pytest's caplog). A global subscriber, because tests run on
/// parallel threads and a scoped one misses callsites another thread cached first.
fn global_logs() -> LogBuf {
    static LOGS: std::sync::OnceLock<LogBuf> = std::sync::OnceLock::new();
    LOGS.get_or_init(|| {
        let buf = LogBuf::default();
        let out = buf.clone();
        let subscriber = tracing_subscriber::fmt().with_writer(move || out.clone()).with_ansi(false).finish();
        let _ = tracing::subscriber::set_global_default(subscriber);
        buf
    })
    .clone()
}

#[tokio::test]
async fn a_client_that_never_answers_pings_is_closed_with_1011_and_one_that_reads_stays() {
    let bus = bound_bus();
    let server = RosbridgeServer::new(bus.clone(), "127.0.0.1", free_port(), DEFAULT_QUEUE_SIZE);
    *server.inner.keepalive.lock() = (Duration::from_millis(150), Duration::from_millis(150));
    server.start().await.unwrap();
    // A client that reads answers every ping (tungstenite queues the pong while reading).
    let mut alive = connect(&server.url()).await;
    send(&mut alive, json!({"op": "subscribe", "topic": "/eve/state"})).await;
    // A client that never reads never answers.
    let mut silent = connect(&server.url()).await;
    assert!(recv_within(&mut alive, 0.8).await.is_err());
    assert_eq!(server.client_count(), 1, "the silent client is closed, the reading one stays");
    bus.publish("/eve/state", msg(json!({"state": "wake"})));
    assert_eq!(recv(&mut alive).await["msg"], json!({"state": "wake"}));
    let mut close = None;
    while let Ok(Some(item)) = tokio::time::timeout(Duration::from_secs(2), silent.next()).await {
        if let Ok(Message::Close(Some(frame))) = item {
            close = Some((u16::from(frame.code), frame.reason.as_str().to_string()));
        }
    }
    assert_eq!(close, Some((1011, "keepalive ping timeout".to_string())));
    server.stop().await;
}

#[test]
fn socket_names_are_shown_as_python_getsockname() {
    assert_eq!(py_sockname("127.0.0.1:9090".parse().unwrap()), "('127.0.0.1', 9090)");
    assert_eq!(py_sockname("[::1]:9090".parse().unwrap()), "('::1', 9090, 0, 0)");
}
