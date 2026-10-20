//! controller.py: the Voice PE next to the StackChan. Input from it reaches the daemon; the robot's state is
//! mirrored to it; its acks and chatter stay out of the robot's link. A fake link stands in for the serial
//! port. Ported from bridge/tests/test_controller.py, plus the controller part of test_music_mode.py.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use super::*;
use serde_json::json;
use tokio::sync::watch;

struct Link {
    on_message: IncomingHandler,
    port: String,
    sent: Mutex<Vec<Value>>,
    connected: AtomicBool,
    evt: watch::Sender<bool>,
    on_boot: Mutex<Option<BootHook>>,
    /// run() returns when this is set (the link was stopped).
    stopped: watch::Sender<bool>,
    fail_sends: AtomicBool,
}

impl Link {
    fn new(on_message: IncomingHandler, port: &str) -> Link {
        Link {
            on_message,
            port: port.to_string(),
            sent: Mutex::new(Vec::new()),
            connected: AtomicBool::new(false),
            evt: watch::channel(false).0,
            on_boot: Mutex::new(None),
            stopped: watch::channel(false).0,
            fail_sends: AtomicBool::new(false),
        }
    }

    fn plug(&self, on: bool) {
        self.connected.store(on, Ordering::SeqCst);
        self.evt.send_replace(on);
    }

    fn sent(&self) -> Vec<Value> {
        self.sent.lock().clone()
    }

    async fn on_message(&self, obj: Value) {
        let Value::Object(map) = obj else { panic!("not an object") };
        (self.on_message)(map).await;
    }

    async fn on_boot(&self) {
        let hook = self.on_boot.lock().clone().expect("on_boot set");
        hook().await;
    }
}

impl ControllerLink for Arc<Link> {
    fn port(&self) -> String {
        self.port.clone()
    }

    fn connected(&self) -> bool {
        self.connected.load(Ordering::SeqCst)
    }

    fn wait_connected(&self) -> BoxFuture<'_, ()> {
        let mut rx = self.evt.subscribe();
        Box::pin(async move {
            let _ = rx.wait_for(|c| *c).await;
        })
    }

    fn send<'a>(&'a self, obj: &'a Value) -> BoxFuture<'a, anyhow::Result<bool>> {
        Box::pin(async move {
            if self.fail_sends.load(Ordering::SeqCst) {
                anyhow::bail!("write failed");
            }
            self.sent.lock().push(obj.clone());
            Ok(true)
        })
    }

    fn run(&self) -> BoxFuture<'static, ()> {
        let mut rx = self.stopped.subscribe();
        Box::pin(async move {
            let _ = rx.wait_for(|s| *s).await;
        })
    }

    fn stop(&self) -> BoxFuture<'_, ()> {
        Box::pin(async move {
            self.stopped.send_replace(true);
        })
    }

    fn set_on_boot(&self, hook: Option<BootHook>) {
        *self.on_boot.lock() = hook;
    }
}

fn make() -> (Arc<Controller>, Arc<Link>, Arc<Mutex<Vec<Value>>>) {
    let got: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = got.clone();
    let on_input: IncomingHandler = Arc::new(move |obj: Map<String, Value>| {
        let sink = sink.clone();
        Box::pin(async move {
            sink.lock().push(Value::Object(obj));
        })
    });
    let made: Arc<Mutex<Option<Arc<Link>>>> = Arc::new(Mutex::new(None));
    let slot = made.clone();
    let factory = move |on_message: IncomingHandler, port: &str| -> Arc<dyn ControllerLink> {
        let link = Arc::new(Link::new(on_message, port));
        *slot.lock() = Some(link.clone());
        Arc::new(link)
    };
    let ctl = Controller::with_link_factory("0A:00:00:00:00:02", on_input, &factory);
    let link = made.lock().clone().expect("link made");
    (Arc::new(ctl), link, got)
}

fn heartbeat() -> Value {
    json!({"total": 2, "running": 1, "waiting": 1, "msg": "approve: Bash"})
}

async fn settle() {
    tokio::time::sleep(Duration::from_millis(50)).await;
}

#[test]
fn test_the_controller_is_named_by_its_usb_serial() {
    let (ctl, link, _) = make();
    assert_eq!(link.port, "usbsn:0A:00:00:00:00:02");
    assert_eq!(ctl.link().port(), "usbsn:0A:00:00:00:00:02");
    let env = HashMap::from([("EVE_CONTROLLER_SERIAL".to_string(), " 0A:00:00:00:00:02 ".to_string())]);
    assert_eq!(controller_serial(Some(&env)).as_deref(), Some("0A:00:00:00:00:02"));
    assert_eq!(controller_serial(Some(&HashMap::new())), None);
}

#[test]
fn test_only_what_the_controller_can_show_is_mirrored() {
    assert!(mirrored(&heartbeat()));
    assert!(mirrored(&json!({"time": [1, 2]})));
    for cmd in ["agent", "listen"] {
        assert!(mirrored(&json!({"cmd": cmd})));
    }
    for cmd in ["sound", "status", "look", "cam", "caption", "move", "expression", "char_begin", "unpair"] {
        assert!(!mirrored(&json!({"cmd": cmd})));
    }
}

#[tokio::test]
async fn test_input_reaches_the_daemon_and_acks_do_not() {
    let (_ctl, link, got) = make();
    for obj in [
        json!({"cmd": "ptt", "on": true}),
        json!({"cmd": "key", "name": "enter"}),
        json!({"cmd": "focus"}),
        json!({"ack": "sound", "ok": true, "n": 0}),
        json!({"ack": "status", "ok": true}),
        json!({"cmd": "explore"}),
    ] {
        link.on_message(obj).await;
    }
    assert_eq!(
        *got.lock(),
        vec![json!({"cmd": "ptt", "on": true}), json!({"cmd": "key", "name": "enter"}), json!({"cmd": "focus"})]
    );
}

#[tokio::test]
async fn test_state_is_sent_while_connected_and_replayed_on_connect() {
    let (ctl, link, _) = make();
    ctl.mirror(&heartbeat()).await; // not connected: kept, not sent
    ctl.mirror(&json!({"cmd": "agent", "state": "thinking"})).await;
    ctl.mirror(&json!({"cmd": "look", "yaw": 10})).await; // not shown: neither kept nor sent
    assert_eq!(link.sent(), Vec::<Value>::new());

    let runner = ctl.clone();
    let task = tokio::spawn(async move { runner.run().await });
    link.plug(true);
    settle().await;
    assert_eq!(link.sent(), vec![SILENT.clone(), heartbeat(), json!({"cmd": "agent", "state": "thinking"})]);

    ctl.mirror(&json!({"cmd": "agent", "state": "speaking"})).await;
    assert_eq!(link.sent().last(), Some(&json!({"cmd": "agent", "state": "speaking"})));

    // a board reboot on an open link replays the latest of each kind
    link.sent.lock().clear();
    link.on_boot().await;
    assert_eq!(link.sent(), vec![SILENT.clone(), heartbeat(), json!({"cmd": "agent", "state": "speaking"})]);
    task.abort();
    let _ = task.await;
}

#[tokio::test]
async fn test_the_beeps_are_the_robots_alone() {
    // The owner turning sound on reaches the robot, never the controller; the controller is silenced on connect.
    let (ctl, link, _) = make();
    let runner = ctl.clone();
    let task = tokio::spawn(async move { runner.run().await });
    link.plug(true);
    settle().await;
    ctl.mirror(&json!({"cmd": "sound", "on": true})).await;
    assert!(!link.sent().contains(&json!({"cmd": "sound", "on": true})));
    assert_eq!(link.sent()[0], json!({"cmd": "sound", "on": false}));
    task.abort();
    let _ = task.await;
}

// ---- test_music_mode.py: the controller keeps the mode for a board that reconnects ----

#[tokio::test]
async fn test_controller_show_keeps_the_mode_only() {
    let (ctl, link, _) = make();
    link.plug(true);
    ctl.show(&json!({"cmd": "music_mode", "on": true})).await;
    ctl.show(&json!({"cmd": "ring_level", "n": 40, "of": 100})).await;
    ctl.replay().await;
    assert!(INPUT_CMDS.contains(&"music"));
    let mut want = Map::new();
    want.insert("music_mode".into(), json!({"cmd": "music_mode", "on": true}));
    assert_eq!(ctl.last(), want);
    assert_eq!(link.sent().last(), Some(&json!({"cmd": "music_mode", "on": true}))); // replayed on (re)connect
}

// ---- extra Rust tests ----

#[tokio::test]
async fn music_input_reaches_the_daemon_and_a_null_cmd_does_not() {
    let (_ctl, link, got) = make();
    link.on_message(json!({"cmd": "music", "click": 1})).await;
    link.on_message(json!({"cmd": null})).await;
    link.on_message(json!({"cmd": 5})).await;
    assert_eq!(*got.lock(), vec![json!({"cmd": "music", "click": 1})]);
}

#[test]
fn a_null_cmd_counts_as_none_and_time_is_its_own_kind() {
    assert!(mirrored(&json!({"cmd": null, "total": 1})));
    assert!(!mirrored(&json!({"cmd": null})));
    assert!(!mirrored(&json!({"msg": "x"})));
    assert!(!mirrored(&json!([1, 2])));
}

#[tokio::test]
async fn the_latest_of_each_kind_is_kept_in_first_seen_order() {
    let (ctl, link, _) = make();
    ctl.mirror(&json!({"time": [1, 2]})).await;
    ctl.mirror(&heartbeat()).await;
    ctl.mirror(&json!({"time": [3, 4]})).await;
    ctl.mirror(&json!({"cmd": "listen", "on": true})).await;
    let keys: Vec<String> = ctl.last().keys().cloned().collect();
    assert_eq!(keys, ["time", "heartbeat", "listen"]);
    assert_eq!(ctl.last()["time"], json!({"time": [3, 4]}));
    link.plug(true);
    ctl.replay().await;
    assert_eq!(
        link.sent(),
        vec![SILENT.clone(), json!({"time": [3, 4]}), heartbeat(), json!({"cmd": "listen", "on": true})]
    );
}

#[tokio::test]
async fn mirror_swallows_send_errors_and_send_audio_does_not() {
    let (ctl, link, _) = make();
    link.plug(true);
    link.fail_sends.store(true, Ordering::SeqCst);
    ctl.mirror(&heartbeat()).await;
    ctl.show(&json!({"cmd": "flash"})).await;
    ctl.replay().await;
    assert!(ctl.send_audio(&json!({"cmd": "pcm", "b": ""})).await.is_err());
    link.plug(false);
    assert!(ctl.send_audio(&json!({"cmd": "pcm", "b": ""})).await.is_ok());
}

#[tokio::test]
async fn send_audio_only_while_connected() {
    let (ctl, link, _) = make();
    ctl.send_audio(&json!({"cmd": "pcm", "b": "AA"})).await.unwrap();
    assert!(link.sent().is_empty());
    link.plug(true);
    ctl.send_audio(&json!({"cmd": "pcm", "b": "AA"})).await.unwrap();
    assert_eq!(link.sent(), vec![json!({"cmd": "pcm", "b": "AA"})]);
    assert!(ctl.last().is_empty());
}

#[tokio::test]
async fn a_reconnect_replays_again_and_stop_ends_run() {
    let (ctl, link, _) = make();
    ctl.mirror(&heartbeat()).await;
    let runner = ctl.clone();
    let task = tokio::spawn(async move { runner.run().await });
    link.plug(true);
    settle().await;
    assert_eq!(link.sent(), vec![SILENT.clone(), heartbeat()]);
    link.plug(false);
    tokio::time::sleep(Duration::from_millis(1700)).await; // the 1 s poll sees it, then the 0.5 s holdoff
    link.sent.lock().clear();
    link.plug(true);
    settle().await;
    assert_eq!(link.sent(), vec![SILENT.clone(), heartbeat()]);
    ctl.stop().await;
    tokio::time::timeout(Duration::from_secs(3), task).await.expect("run ends after stop").unwrap();
}

#[tokio::test]
async fn dropping_run_drops_the_link_task() {
    let (ctl, link, _) = make();
    let runner = ctl.clone();
    let task = tokio::spawn(async move { runner.run().await });
    settle().await;
    // The link's run() future holds a receiver of `stopped`; once run() is cancelled it is gone.
    assert_eq!(link.stopped.receiver_count(), 1);
    task.abort();
    let _ = task.await;
    settle().await;
    assert_eq!(link.stopped.receiver_count(), 0);
}

#[tokio::test]
async fn the_real_link_is_an_eve_serial_on_the_usbsn_port() {
    let on_input: IncomingHandler = Arc::new(|_obj| Box::pin(async {}));
    let ctl = Controller::new("0A:00:00:00:00:02", on_input);
    assert_eq!(ctl.link().port(), "usbsn:0A:00:00:00:00:02");
    assert!(!ctl.connected());
    ctl.mirror(&heartbeat()).await;
    assert_eq!(ctl.last().keys().collect::<Vec<_>>(), ["heartbeat"]);
}

#[test]
fn python_strip_also_trims_the_separators() {
    let env = HashMap::from([("EVE_CONTROLLER_SERIAL".to_string(), "\u{1f} \t\n".to_string())]);
    assert_eq!(controller_serial(Some(&env)), None);
}
