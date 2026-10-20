//! Port of bridge/tests/test_memory_bus.py and the configured() / TOPICS parts of
//! test_daemon_membus.py, test_follow.py and test_rosbridge.py.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use serde_json::json;

use super::*;

fn msg(v: Value) -> Msg {
    match v {
        Value::Object(m) => m,
        other => panic!("not an object: {other}"),
    }
}

/// Captures what `tracing` logs while `f` runs (pytest's caplog).
fn capture_logs(f: impl FnOnce()) -> String {
    #[derive(Clone, Default)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf::default();
    let out = buf.clone();
    let subscriber = tracing_subscriber::fmt().with_writer(move || out.clone()).with_ansi(false).finish();
    tracing::subscriber::with_default(subscriber, f);
    let bytes = buf.0.lock().clone();
    String::from_utf8_lossy(&bytes).into_owned()
}

#[test]
fn test_publish_reaches_exact_and_prefix_subscribers_only() {
    let bus = MemoryBus::with_clock(|| 1.0);
    let got: Arc<Mutex<Vec<(&'static str, Msg)>>> = Arc::default();
    let g = got.clone();
    bus.subscribe("/eve/memory/lesson", move |_, m| g.lock().push(("exact", m.clone())));
    let g = got.clone();
    bus.subscribe("/eve/memory/*", move |_, m| g.lock().push(("prefix", m.clone())));
    let g = got.clone();
    bus.subscribe("/eve/state", move |_, m| g.lock().push(("state", m.clone())));
    let event = bus.publish("/eve/memory/lesson", msg(json!({"action": "hint"})));
    assert!(event.time == 1.0 && event.msg == msg(json!({"action": "hint"})));
    assert_eq!(
        *got.lock(),
        vec![("exact", msg(json!({"action": "hint"}))), ("prefix", msg(json!({"action": "hint"})))]
    );
}

#[test]
fn a_literal_star_topic_reaches_its_exact_subscriber_once() {
    let bus = MemoryBus::new();
    let got: Arc<Mutex<Vec<String>>> = Arc::default();
    let g = got.clone();
    bus.subscribe("/eve/*", move |t, _| g.lock().push(t.to_string()));
    bus.publish("/eve/*", Msg::new());
    bus.publish("/eve/state", Msg::new());
    bus.publish("/claude/observation", Msg::new());
    assert_eq!(*got.lock(), vec!["/eve/*".to_string(), "/eve/state".to_string()]);
}

#[test]
fn test_unsubscribe_stops_delivery() {
    let bus = MemoryBus::new();
    let got: Arc<Mutex<Vec<Msg>>> = Arc::default();
    let g = got.clone();
    let sub = bus.subscribe("/eve/state", move |_, m| g.lock().push(m.clone()));
    bus.publish("/eve/state", msg(json!({"state": "wake"})));
    bus.unsubscribe(&sub);
    bus.publish("/eve/state", msg(json!({"state": "idle"})));
    assert_eq!(*got.lock(), vec![msg(json!({"state": "wake"}))]);
    assert_eq!(bus.subscription_count(), 0);
}

#[test]
fn test_a_subscriber_that_raises_is_logged_and_the_others_still_run() {
    let bus = MemoryBus::new();
    let got: Arc<Mutex<Vec<Msg>>> = Arc::default();
    bus.subscribe_fallible("/eve/state", |_, _| Err(anyhow::anyhow!("boom")));
    bus.subscribe("/eve/state", |_, _| panic!("boom"));
    let g = got.clone();
    bus.subscribe("/eve/state", move |_, m| g.lock().push(m.clone()));
    let logs = capture_logs(|| {
        bus.publish("/eve/state", msg(json!({"state": "wake"})));
    });
    assert_eq!(*got.lock(), vec![msg(json!({"state": "wake"}))]);
    assert_eq!(logs.matches("subscriber failed").count(), 2, "{logs}");
    assert!(logs.contains("memory bus: subscriber failed on /eve/state"), "{logs}");
    assert!(logs.contains("boom"), "{logs}");
}

#[tokio::test]
async fn test_publish_from_another_thread_delivers_on_the_bound_loop() {
    let bus = MemoryBus::new();
    bus.bind_loop(Handle::current());
    let seen: Arc<Mutex<Vec<(std::thread::ThreadId, Msg)>>> = Arc::default();
    let done = Arc::new(tokio::sync::Notify::new());
    let (s, d) = (seen.clone(), done.clone());
    bus.subscribe("/eve/state", move |_, m| {
        s.lock().push((std::thread::current().id(), m.clone()));
        d.notify_one();
    });
    let b = bus.clone();
    let audio = std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || {
            b.publish("/eve/state", msg(json!({"state": "thinking"})));
            std::thread::current().id()
        })
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), done.notified()).await.unwrap();
    let audio_id = audio.join().unwrap();
    let here = std::thread::current().id();
    assert_ne!(audio_id, here);
    // the loop thread, not "audio"
    assert_eq!(*seen.lock(), vec![(here, msg(json!({"state": "thinking"})))]);
}

#[tokio::test]
async fn publish_on_the_bound_loop_delivers_inline() {
    let bus = MemoryBus::new();
    bus.bind_loop(Handle::current());
    let got: Arc<Mutex<Vec<Msg>>> = Arc::default();
    let g = got.clone();
    bus.subscribe("/eve/state", move |_, m| g.lock().push(m.clone()));
    bus.publish("/eve/state", msg(json!({"state": "wake"})));
    assert_eq!(got.lock().len(), 1); // before any await
}

#[tokio::test]
async fn test_an_async_subscriber_is_scheduled_on_the_loop() {
    let bus = MemoryBus::new();
    bus.bind_loop(Handle::current());
    let seen: Arc<Mutex<Vec<Msg>>> = Arc::default();
    let s = seen.clone();
    bus.subscribe_async("/eve/state", move |_, m| {
        let s = s.clone();
        async move {
            s.lock().push(m);
        }
    });
    bus.publish("/eve/state", msg(json!({"state": "wake"})));
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(*seen.lock(), vec![msg(json!({"state": "wake"}))]);
    assert_eq!(bus.dropped(), 0);
}

#[test]
fn an_async_subscriber_without_a_loop_is_dropped_and_counted() {
    let bus = MemoryBus::new();
    let seen: Arc<Mutex<Vec<Msg>>> = Arc::default();
    let s = seen.clone();
    bus.subscribe_async("/eve/state", move |_, m| {
        let s = s.clone();
        async move {
            s.lock().push(m);
        }
    });
    bus.publish("/eve/state", msg(json!({"state": "wake"})));
    assert!(seen.lock().is_empty());
    assert_eq!(bus.dropped(), 1);
}

#[test]
fn a_closed_loop_delivers_inline_and_drops_async_subscribers() {
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let bus = MemoryBus::new();
    bus.bind_loop(rt.handle().clone());
    drop(rt);
    let got: Arc<Mutex<Vec<Msg>>> = Arc::default();
    let g = got.clone();
    bus.subscribe("/eve/state", move |_, m| g.lock().push(m.clone()));
    bus.subscribe_async("/eve/state", |_, _| async {});
    bus.publish("/eve/state", msg(json!({"state": "wake"})));
    assert_eq!(*got.lock(), vec![msg(json!({"state": "wake"}))]);
    assert_eq!(bus.dropped(), 1);
}

#[tokio::test]
async fn test_services_are_awaited_and_missing_ones_raise() {
    let bus = MemoryBus::new();
    bus.register_service("/eve/memory/recall", |args: Msg| async move {
        Ok(msg(json!({"results": [args["query"].clone()]})))
    });
    assert!(bus.has_service("/eve/memory/recall"));
    assert_eq!(
        bus.call_service("/eve/memory/recall", msg(json!({"query": "mug"}))).await.unwrap(),
        msg(json!({"results": ["mug"]}))
    );
    match bus.call_service("/nope", Msg::new()).await {
        Err(ServiceError::NoSuchService(name)) => assert_eq!(name, "/nope"),
        other => panic!("missing service did not raise: {other:?}"),
    }
    assert_eq!(ServiceError::NoSuchService("/nope".into()).to_string(), "no such service: /nope");
    bus.register_service("/eve/memory/broken", |_| async { Err(anyhow::anyhow!("worker down")) });
    let err = bus.call_service("/eve/memory/broken", Msg::new()).await.unwrap_err();
    assert!(matches!(err, ServiceError::Failed(_)) && err.to_string().contains("worker down"));
}

#[test]
fn test_recent_keeps_a_bounded_ring_per_topic() {
    let bus = MemoryBus::new();
    for i in 0..RECENT_PER_TOPIC + 5 {
        bus.publish("/eve/state", msg(json!({"i": i})));
    }
    let recent = bus.recent("/eve/state");
    assert!(recent.len() == RECENT_PER_TOPIC && recent.last().unwrap().msg == msg(json!({"i": RECENT_PER_TOPIC + 4})));
    assert_eq!(bus.recent("/eve/memory/lesson"), vec![]);
    assert_eq!(bus.recent_n("/eve/state", 2)[0].msg, msg(json!({"i": RECENT_PER_TOPIC + 3})));
    // Python's [-n:] slice: 0 is the whole ring, a negative n drops from the front.
    assert_eq!(bus.recent_n("/eve/state", 0).len(), RECENT_PER_TOPIC);
    assert_eq!(bus.recent_n("/eve/state", -18).len(), 2);
    assert_eq!(bus.recent_n("/eve/state", 1000).len(), RECENT_PER_TOPIC);
}

#[test]
fn test_topic_and_service_catalogue_is_ros_shaped() {
    assert!(TOPICS.iter().all(|(t, typ)| t.starts_with('/') && typ.contains('/')));
    assert!(services_map().contains_key("/eve/memory/recall"));
    assert_eq!(MemoryBus::new().topics(), topics_map());
    assert_eq!(MemoryBus::new().services(), services_map());
    assert_eq!(topics_map().len(), TOPICS.len());
}

// ---- test_daemon_membus.py / test_follow.py: the catalogue ---------------------------------

#[test]
fn test_nothing_said_in_a_conversation_is_published() {
    let topics = topics_map();
    assert!(!topics.contains_key("/eve/memory/conversation"));
    assert!(topics.contains_key("/eve/memory/observation")); // the control: the diary topic is still there
    assert_eq!(topics["/eve/presence"], "eve_msgs/Presence");
}

// ---- test_daemon_membus.py: configured() ----------------------------------------------------

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

#[test]
fn test_everything_is_off_by_default() {
    assert_eq!(configured(Some(&env(&[]))), BusConfig::default());
}

#[test]
fn test_env_flags_and_mirror_default() {
    let cfg = configured(Some(&env(&[
        ("EVE_ROSBRIDGE", "1"),
        ("EVE_ROSBRIDGE_PORT", "9999"),
        ("EVE_ROSBRIDGE_HOST", "0.0.0.0"),
        ("EVE_CLAUDE_MEM", "true"),
        ("EVE_CLAUDE_MEM_URL", "http://127.0.0.1:37701"),
    ])));
    assert_eq!(
        cfg,
        BusConfig {
            rosbridge: true,
            rosbridge_host: "0.0.0.0".into(),
            rosbridge_port: 9999,
            claude_mem: true,
            claude_mem_url: Some("http://127.0.0.1:37701".into()),
            mirror: true,
        }
    );
    assert!(!configured(Some(&env(&[("EVE_CLAUDE_MEM", "on"), ("EVE_CLAUDE_MEM_MIRROR", "off")]))).mirror);
    let logs = capture_logs(|| {
        assert_eq!(configured(Some(&env(&[("EVE_ROSBRIDGE", "1"), ("EVE_ROSBRIDGE_PORT", "lots")]))).rosbridge_port, 9090);
    });
    assert!(logs.contains("memory bus: EVE_ROSBRIDGE_PORT='lots' is not a number; using 9090"), "{logs}");
}

#[test]
fn configured_strips_and_lowercases_like_python() {
    let cfg = configured(Some(&env(&[
        ("EVE_ROSBRIDGE", " YES "),
        ("EVE_ROSBRIDGE_HOST", "   "),
        ("EVE_ROSBRIDGE_PORT", " +9_091 "),
        ("EVE_CLAUDE_MEM", "nope"),
        ("EVE_CLAUDE_MEM_URL", "  "),
        ("EVE_CLAUDE_MEM_MIRROR", "On"),
    ])));
    assert_eq!(
        cfg,
        BusConfig {
            rosbridge: true,
            rosbridge_host: DEFAULT_ROSBRIDGE_HOST.into(),
            rosbridge_port: 9091,
            claude_mem: false,
            claude_mem_url: None,
            mirror: true,
        }
    );
}

#[test]
fn python_int_matches_int() {
    assert_eq!(python_int("9090"), Some(9090));
    assert_eq!(python_int("-1"), Some(-1));
    assert_eq!(python_int("1_000"), Some(1000));
    assert_eq!(python_int("007"), Some(7));
    for bad in ["", "+", "_1", "1_", "1__0", "9.0", "0x10", "lots"] {
        assert_eq!(python_int(bad), None, "{bad}");
    }
}

#[test]
fn py_repr_quotes_like_python() {
    assert_eq!(py_repr("lots"), "'lots'");
    assert_eq!(py_repr("it's"), "\"it's\"");
    assert_eq!(py_repr("a\\b"), "'a\\\\b'");
}

// ---- test_rosbridge.py: what the server reads off the bus -----------------------------------

#[test]
fn subscription_count_tracks_subscribe_and_unsubscribe() {
    let bus = MemoryBus::new();
    let a = bus.subscribe("/eve/state", |_, _| {});
    let _b = bus.subscribe("/eve/memory/lesson", |_, _| {});
    assert_eq!(bus.subscription_count(), 2);
    bus.unsubscribe(&a);
    bus.unsubscribe(&a);
    assert_eq!(bus.subscription_count(), 1);
}

#[test]
fn a_subscriber_can_unsubscribe_itself_while_being_delivered() {
    let bus = MemoryBus::new();
    let slot: Arc<Mutex<Option<Subscription>>> = Arc::default();
    let (b, s) = (bus.clone(), slot.clone());
    let sub = bus.subscribe("/eve/state", move |_, _| {
        if let Some(sub) = s.lock().take() {
            b.unsubscribe(&sub);
        }
    });
    *slot.lock() = Some(sub);
    bus.publish("/eve/state", Msg::new());
    assert_eq!(bus.subscription_count(), 0);
}
