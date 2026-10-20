//! obsbot.py's tests: the camera over USB (a fake UsbControl that behaves as the Tiny 3 measured on 2026-10-11)
//! and OBSBOT's own features over OSC (a fake OBSBOT Center on a free UDP port, answering the way the real one did).

use std::net::UdpSocket;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use serde_json::{json, Value};

use super::*;

#[derive(Debug)]
struct State {
    name: String,
    pan: f64,
    tilt: f64,
    zoom_level: i64,
    asleep: bool,
    unplugged: bool,
    calls: Vec<Call>,
}

#[derive(Debug, Clone, PartialEq)]
enum Call {
    Wake,
    Point(f64, f64),
    Turn(&'static str),
    Zoom(i64),
}

/// UsbCamera's surface. Asleep, it reads tilt -79 and ignores moves, as the camera does.
#[derive(Clone)]
struct FakeUsb(Arc<Mutex<State>>, String);

impl FakeUsb {
    fn new() -> Self {
        Self::named("OBSBOT Tiny 3")
    }

    fn named(name: &str) -> Self {
        FakeUsb(
            Arc::new(Mutex::new(State {
                name: name.into(),
                pan: 0.0,
                tilt: 0.0,
                zoom_level: 0,
                asleep: false,
                unplugged: false,
                calls: Vec::new(),
            })),
            String::new(),
        )
    }

    fn calls(&self) -> Vec<Call> {
        self.0.lock().calls.clone()
    }
}

impl UsbControl for FakeUsb {
    fn read(&mut self) -> Option<Reply> {
        let s = self.0.lock();
        if s.unplugged {
            self.1 = NO_CAMERA_LINE.into();
            return None;
        }
        let v = json!({"ok": true, "name": s.name, "pan": s.pan, "tilt": if s.asleep { -79.0 } else { s.tilt },
                       "zoom": s.zoom_level, "asleep": s.asleep});
        v.as_object().cloned()
    }

    fn wake(&mut self) -> bool {
        let mut s = self.0.lock();
        s.calls.push(Call::Wake);
        s.asleep = false;
        !s.unplugged
    }

    fn point(&mut self, pan: f64, tilt: f64) -> bool {
        let mut s = self.0.lock();
        s.calls.push(Call::Point(pan, tilt));
        if !s.asleep {
            s.pan = pan;
            s.tilt = tilt;
        }
        true
    }

    fn turn(&mut self, direction: Direction, _speed: i64) -> bool {
        let mut s = self.0.lock();
        s.calls.push(Call::Turn(direction.as_str()));
        let d = match direction {
            Direction::Left => (5.0, 0.0),
            Direction::Right => (-5.0, 0.0),
            Direction::Up => (0.0, 5.0),
            Direction::Down => (0.0, -5.0),
            Direction::Stop => (0.0, 0.0),
        };
        s.pan += d.0;
        s.tilt += d.1;
        true
    }

    fn zoom(&mut self, level: i64) -> bool {
        let mut s = self.0.lock();
        s.calls.push(Call::Zoom(level));
        s.zoom_level = level;
        true
    }

    fn why(&self) -> &str {
        &self.1
    }
}

/// `time.sleep` patched out; `time.monotonic` stays real, as in the Python tests.
struct Quick;

impl Clock for Quick {
    fn monotonic(&self) -> f64 {
        SystemClock.monotonic()
    }

    fn sleep(&self, _secs: f64) {
        std::thread::yield_now();
    }
}

type Got = Arc<Mutex<Vec<(String, Vec<OscValue>)>>>;

struct FakeCenter {
    port: u16,
    got: Got,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl FakeCenter {
    fn new(usb: Option<FakeUsb>) -> Self {
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = sock.local_addr().unwrap().port();
        let got: Got = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let (g, st) = (got.clone(), stop.clone());
        let thread = std::thread::spawn(move || {
            sock.set_read_timeout(Some(Duration::from_millis(100)))
                .unwrap();
            let mut buf = vec![0u8; 65536];
            while !st.load(Ordering::SeqCst) {
                let Ok((n, src)) = sock.recv_from(&mut buf) else {
                    continue;
                };
                let (address, args) = decode(&buf[..n]).unwrap();
                g.lock().push((address.clone(), args.clone()));
                let name = address.rsplit('/').next().unwrap_or("").to_string();
                let reply = |addr: String, vals: Vec<OscValue>| {
                    sock.send_to(&encode(&addr, &vals).unwrap(), src).unwrap();
                };
                match name.as_str() {
                    "Connected" => reply(format!("{G}ConnectedResp"), vec![1.into()]),
                    "GetPresetPositionInfo" if args.len() == 1 => reply(
                        format!("{T}PresetPositionInfo"),
                        vec![
                            1.into(),
                            "Desk".into(),
                            0.into(),
                            "Add".into(),
                            0.into(),
                            "Add".into(),
                        ],
                    ),
                    "GetAiTrackingInfo" if args.len() == 1 => {
                        reply(format!("{T}AiTrackingInfo"), vec![0.into()])
                    }
                    "GetDeviceInfo" if args.len() == 1 => reply(
                        format!("{G}DeviceInfo"),
                        vec![
                            1.into(),
                            "OBSBOT Tiny 3".into(),
                            0.into(),
                            "".into(),
                            0.into(),
                            "".into(),
                            0.into(),
                            "".into(),
                            0.into(),
                            1.into(),
                            18.into(),
                        ],
                    ),
                    "GetZoomInfo" if args.len() == 1 => reply(
                        format!("{G}ZoomInfoResp"),
                        vec![25.into(), OscValue::Float(-1.0)],
                    ),
                    "WakeSleep" => {
                        if let Some(u) = &usb {
                            u.0.lock().asleep = !args.get(1).is_some_and(OscValue::truthy);
                        }
                    }
                    _ => {}
                }
            }
        });
        FakeCenter {
            port,
            got,
            stop,
            thread: Some(thread),
        }
    }

    /// A fire-and-forget command reaches the fake on its own thread; wait for it.
    fn arrives(&self, address: &str, args: Vec<OscValue>) -> bool {
        let end = Instant::now() + Duration::from_secs(1);
        let want = (address.to_string(), args);
        while Instant::now() < end {
            if self.got.lock().contains(&want) {
                return true;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        false
    }
}

impl Drop for FakeCenter {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn opened(usb: &FakeUsb, port: u16, running: bool) -> Obsbot {
    Obsbot::options()
        .port(port)
        .app_running(move || running)
        .usb(usb.clone())
        .clock(Quick)
        .open()
        .unwrap()
}

/// No OBSBOT Center.
fn cam(usb: &FakeUsb) -> Obsbot {
    opened(usb, 1, false)
}

fn cam_app(usb: &FakeUsb, center: &FakeCenter) -> Obsbot {
    opened(usb, center.port, true)
}

fn line(res: &Res) -> &str {
    res.get("line").and_then(Value::as_str).unwrap_or("")
}

fn ok(res: &Res) -> bool {
    res.get("ok") == Some(&Value::Bool(true))
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

#[test]
fn test_osc_round_trip() {
    let data = encode(
        "/OBSBOT/x",
        &[3.into(), OscValue::Float(-1.5), "Tiny 3".into()],
    )
    .unwrap();
    assert_eq!(data.len() % 4, 0);
    assert_eq!(
        decode(&data).unwrap(),
        (
            "/OBSBOT/x".to_string(),
            vec![
                OscValue::Int(3),
                OscValue::Float(-1.5),
                OscValue::Str("Tiny 3".into())
            ]
        )
    );
}

#[test]
fn test_osc_bytes_padding_and_errors() {
    // Python: encode("/a", 1) == b"/a\0\0,i\0\0\0\0\0\x01"; a 4-byte address still gets its four nulls
    assert_eq!(
        encode("/a", &[1.into()]).unwrap(),
        b"/a\0\0,i\0\0\0\0\0\x01".to_vec()
    );
    assert_eq!(&encode("/abc", &[]).unwrap()[..8], b"/abc\0\0\0\0");
    assert_eq!(
        encode("/x", &[OscValue::Int(1 << 31)]),
        Err(OscError::IntRange)
    );
    assert_eq!(
        encode("/x", &[OscValue::Float(1e39)]),
        Err(OscError::FloatRange)
    );
    assert!(encode("/x", &[OscValue::Float(f64::INFINITY)]).is_ok());
    assert_eq!(decode(b"/x"), Err(OscError::NoTerminator));
    assert_eq!(decode(b"/x\0\0,i\0\0\0\0"), Err(OscError::Short));
    // no tag string: no values; an unknown tag stops the read
    assert_eq!(decode(b"/x\0\0").unwrap(), ("/x".to_string(), vec![]));
    let data = b"/x\0\0,ib\0\0\0\0\x07".to_vec();
    assert_eq!(decode(&data).unwrap().1, vec![OscValue::Int(7)]);
    // floats come back rounded to 2 places, as Python's round(x, 2)
    let data = encode("/x", &[OscValue::Float(0.1)]).unwrap();
    assert_eq!(decode(&data).unwrap().1, vec![OscValue::Float(0.1)]);
    assert_eq!(
        values_repr(&[1.into(), "Desk".into(), OscValue::Float(0.5), "it's".into()]),
        "[1, 'Desk', 0.5, \"it's\"]"
    );
}

#[test]
fn test_status_over_usb_alone() {
    let usb = FakeUsb::new();
    let mut c = cam(&usb);
    assert_eq!(c.ensure(), None);
    let st = c.status();
    assert!(ok(&st) && st["camera"] == "Tiny 3" && st["awake"] == true && st["app"] == false);
    assert_eq!(
        (st["pan"].clone(), st["pitch"].clone(), st["zoom"].clone()),
        (json!(0), json!(0), json!(0))
    );
    assert_eq!(line(&st), "Tiny 3 awake; pan 0°, pitch 0°; zoom 0%.");
    c.close();
}

#[test]
fn test_status_asleep_and_unplugged() {
    let usb = FakeUsb::new();
    let mut c = cam(&usb);
    usb.0.lock().asleep = true;
    let st = c.status();
    assert!(st["awake"] == false && st["pan"].is_null() && line(&st) == "Tiny 3 asleep; zoom 0%.");
    usb.0.lock().unplugged = true;
    assert_eq!(
        Value::Object(c.status()),
        json!({"ok": false, "line": NO_CAMERA_LINE})
    );
    let mut fresh = cam(&usb);
    assert_eq!(fresh.ensure().as_deref(), Some(NO_CAMERA_LINE));
    fresh.close();
    c.close();
}

#[test]
fn test_move_turns_pitch_into_tilt_and_reads_back() {
    let usb = FakeUsb::new();
    let mut c = cam(&usb);
    let res = c.move_to(Some(45.0), Some(10.0)); // pitch + is down: the camera's tilt -10
    assert!(ok(&res) && res["pan"] == 45 && res["pitch"] == 10);
    assert!(usb.calls().contains(&Call::Point(45.0, -10.0)));
}

#[test]
fn test_move_wakes_a_sleeping_camera_first() {
    let usb = FakeUsb::new();
    let mut c = cam(&usb);
    usb.0.lock().asleep = true;
    let res = c.move_to(Some(20.0), Some(0.0));
    assert!(ok(&res));
    assert_eq!(usb.calls()[..2], [Call::Wake, Call::Point(20.0, 0.0)]);
    // tilt 0 goes out as +0, not -0 (the helper prints "0.00")
    assert!(usb
        .calls()
        .iter()
        .any(|c| matches!(c, Call::Point(_, t) if *t == 0.0 && t.is_sign_positive())));
}

#[test]
fn test_move_keeps_the_axis_not_given_and_clamps() {
    let usb = FakeUsb::new();
    let mut c = cam(&usb);
    {
        let mut s = usb.0.lock();
        s.pan = 20.0;
        s.tilt = 5.0;
    }
    assert_eq!(c.move_to(None, Some(15.0))["pan"], 20);
    c.move_to(Some(500.0), Some(-500.0));
    assert!(usb.calls().contains(&Call::Point(129.0, TILT_LIMIT)));
}

#[test]
fn test_move_rounds_half_to_even_and_refuses_when_it_wont_wake() {
    let usb = FakeUsb::new();
    let mut c = cam(&usb);
    c.move_to(Some(2.5), Some(-3.5));
    assert!(usb.calls().contains(&Call::Point(2.0, 4.0)));
    // a camera that doesn't wake refuses with its reason, else the stock line
    let usb2 = FakeUsb::new();
    let mut c2 = cam(&usb2);
    usb2.0.lock().unplugged = true;
    assert_eq!(
        line(&c2.move_to(Some(10.0), None)),
        "The camera is asleep and didn't wake."
    );
}

#[test]
fn test_nudge_turns_then_stops() {
    let usb = FakeUsb::new();
    let mut c = cam(&usb);
    let res = c.nudge("left", TURN_SPEED, 0.5);
    let calls = usb.calls();
    assert_eq!(
        calls[calls.len() - 2..],
        [Call::Turn("left"), Call::Turn("stop")]
    );
    assert_eq!(res["pan"], 5);
    let bad = c.nudge("sideways", TURN_SPEED, 0.5);
    assert!(!ok(&bad));
    assert_eq!(line(&bad), "Direction is one of: up, down, left, right.");
    assert!(!ok(&c.nudge("stop", TURN_SPEED, 0.5))); // "stop" is not a nudge
}

#[test]
fn test_zoom_and_center() {
    let usb = FakeUsb::new();
    let mut c = cam(&usb);
    assert_eq!(line(&c.zoom(60)), "Zoom 60%.");
    usb.0.lock().pan = 30.0;
    let res = c.reset();
    assert_eq!(line(&res), "Centered.");
    assert_eq!(usb.0.lock().pan, 0.0);
    assert_eq!(
        res.keys().collect::<Vec<_>>(),
        ["ok", "line", "pan", "pitch"]
    );
    assert!(c.zoom(500).get("zoom") == Some(&json!(100)));
}

#[test]
fn test_app_features_say_the_app_is_needed() {
    let usb = FakeUsb::new();
    let mut c = cam(&usb);
    let want = json!({"ok": false, "line": APP_NEEDED_LINE});
    for res in [
        c.track(true),
        c.mode("desk"),
        c.preset(1),
        c.wake(false),
        c.fov(78),
        c.mirror(true),
    ] {
        assert_eq!(Value::Object(res), want);
    }
    assert_eq!(line(&c.wake(true)), "Tiny 3 awake."); // waking is USB's: no app needed
}

#[test]
fn test_app_features_go_to_obsbot_center_when_it_runs() {
    let usb = FakeUsb::new();
    let center = FakeCenter::new(Some(usb.clone()));
    let mut c = cam_app(&usb, &center);
    assert_eq!(line(&c.track(true)), "Tracking on.");
    assert!(center.arrives(&format!("{T}ToggleAILock"), vec![0.into(), 1.into()]));
    assert!(!ok(&c.preset(2))); // not saved
    assert_eq!(line(&c.preset(1)), "Going to Desk.");
    assert!(center.arrives(&format!("{T}TriggerPreset"), vec![0.into(), 0.into()]));
    assert_eq!(line(&c.wake(false)), "Tiny 3 asleep.");
    assert!(usb.0.lock().asleep);
    c.close();
}

#[test]
fn test_status_with_the_app_lists_presets_and_info() {
    let usb = FakeUsb::new();
    let center = FakeCenter::new(Some(usb.clone()));
    let mut c = cam_app(&usb, &center);
    let st = c.status();
    assert_eq!(
        line(&st),
        "Tiny 3 awake; pan 0°, pitch 0°; zoom 0%; presets: Desk."
    );
    assert_eq!(st["app"], true);
    assert_eq!(st["locked"], false);
    assert_eq!(
        st["presets"],
        json!([{"n": 1, "set": true, "name": "Desk"}, {"n": 2, "set": false, "name": ""},
               {"n": 3, "set": false, "name": ""}])
    );
    let info = c.info().unwrap();
    assert_eq!(
        Value::Object(info),
        json!({"devices": [{"index": 0, "connected": true, "name": "OBSBOT Tiny 3"}], "selected": 0,
               "awake": true, "type": 18})
    );
    assert_eq!(line(&c.fov(78)), "Field of view 78°.");
    assert!(center.arrives(&format!("{G}SetView"), vec![0.into(), 1.into()]));
    assert_eq!(line(&c.focus("150")), "Manual focus 100.");
    assert!(center.arrives(&format!("{G}SetAutoFocus"), vec![0.into(), 0.into()]));
    assert!(center.arrives(&format!("{G}SetManualFocus"), vec![0.into(), 100.into()]));
    assert_eq!(line(&c.focus("auto")), "Autofocus on.");
    assert_eq!(line(&c.focus("near")), "Focus is auto or 0-100.");
    assert_eq!(line(&c.mode("desk")), "AI mode desk.");
    assert!(center.arrives(&format!("{T}SetAiMode"), vec![0.into(), 3.into()]));
    assert_eq!(
        line(&c.mode("party")),
        "AI mode is one of: single, group, voice, desk, hand, whiteboard."
    );
    assert_eq!(line(&c.framing("motion")), "Framing motion.");
    assert_eq!(line(&c.track_speed("fast")), "Tracking speed fast.");
    assert_eq!(
        line(&c.track_speed("warp")),
        "Tracking speed is one of: slow, standard, fast."
    );
    assert_eq!(line(&c.mirror(false)), "Not mirrored.");
    assert_eq!(line(&c.fov(90)), "Field of view is 86, 78 or 65.");
    assert_eq!(line(&c.preset(4)), "Preset is 1, 2 or 3.");
    c.close();
}

#[test]
fn test_ai_mode_refuses_another_model() {
    let other = FakeUsb::named("OBSBOT Tiny 2");
    let mut o = opened(&other, 1, true);
    assert!(line(&o.mode("desk")).contains("Tiny 3"));
    assert_eq!(
        line(&o.track_speed("slow")),
        "These settings use the Tiny 3's numbering; this camera is a OBSBOT Tiny 2."
    );
    o.close();
}

#[test]
fn test_a_running_app_with_osc_off_is_named() {
    let usb = FakeUsb::new();
    let mut o = opened(&usb, 1, true);
    assert_eq!(line(&o.track(true)), OFF_REASON);
    o.close();
}

#[test]
fn test_close_says_disconnected_only_to_an_app_it_reached() {
    let usb = FakeUsb::new();
    let center = FakeCenter::new(Some(usb.clone()));
    let mut c = cam_app(&usb, &center);
    c.track(false);
    c.close();
    assert!(center.arrives(&format!("{G}Disconnected"), vec![0.into()]));
}

fn maker(usb: &FakeUsb, port: u16, running: bool) -> impl Fn(i64) -> io::Result<Obsbot> {
    let usb = usb.clone();
    move |device| {
        Obsbot::options()
            .port(port)
            .device(device)
            .app_running(move || running)
            .usb(usb.clone())
            .clock(Quick)
            .open()
    }
}

#[test]
fn test_cli_json_after_the_subcommand() {
    let usb = FakeUsb::new();
    let mut out: Vec<String> = Vec::new();
    assert_eq!(
        cli(
            &s(&["status", "--json"]),
            &mut |l| out.push(l.into()),
            &maker(&usb, 1, false)
        ),
        0
    );
    let v: Value = serde_json::from_str(&out[0]).unwrap();
    assert_eq!(v["camera"], "Tiny 3");
    // json.dumps's separators, and the keys in the order the status builds them
    assert!(out[0].starts_with(
        "{\"ok\": true, \"line\": \"Tiny 3 awake; pan 0°, pitch 0°; zoom 0%.\", \"camera\""
    ));
}

#[test]
fn test_cli_move_nudge_and_exit_codes() {
    let usb = FakeUsb::new();
    let mut out: Vec<String> = Vec::new();
    let make = maker(&usb, 1, false);
    assert_eq!(
        cli(
            &s(&["move", "--pan", "30", "--pitch", "-10"]),
            &mut |l| out.push(l.into()),
            &make
        ),
        0
    );
    assert_eq!(out.last().unwrap(), "Pan 30°, pitch -10°.");
    assert_eq!(
        cli(&s(&["track", "on"]), &mut |l| out.push(l.into()), &make),
        1
    );
    assert_eq!(out.last().unwrap(), APP_NEEDED_LINE);
    assert_eq!(
        cli(
            &s(&["nudge", "up", "--seconds", "0.1"]),
            &mut |l| out.push(l.into()),
            &make
        ),
        0
    );
    assert_eq!(out.last().unwrap(), "Moved up. Pan 30°, pitch -15°.");
    assert_eq!(
        cli(&s(&["zoom", "40"]), &mut |l| out.push(l.into()), &make),
        0
    );
    assert_eq!(out.last().unwrap(), "Zoom 40%.");
    assert_eq!(cli(&s(&["stop"]), &mut |l| out.push(l.into()), &make), 0);
    assert_eq!(out.last().unwrap(), "Stopped.");
    // argparse errors: an unknown choice, no subcommand
    assert_eq!(
        cli(
            &s(&["nudge", "sideways"]),
            &mut |l| out.push(l.into()),
            &make
        ),
        2
    );
    assert_eq!(cli(&s(&[]), &mut |l| out.push(l.into()), &make), 2);
    assert_eq!(cli(&s(&["--help"]), &mut |l| out.push(l.into()), &make), 0);
}

#[test]
fn test_cli_device_after_the_subcommand_goes_to_the_camera() {
    let usb = FakeUsb::new();
    let center = FakeCenter::new(Some(usb.clone()));
    let mut out: Vec<String> = Vec::new();
    assert_eq!(
        cli(
            &s(&["track", "on", "--device", "2"]),
            &mut |l| out.push(l.into()),
            &maker(&usb, center.port, true)
        ),
        0
    );
    assert!(center.arrives(&format!("{T}ToggleAILock"), vec![2.into(), 1.into()]));
    assert_eq!(
        reorder(&s(&["status", "--json", "--device", "1", "--json"])),
        s(&["--json", "--json", "--device", "1", "status"])
    );
    // "--device" first, or with no value after it, stays where it is
    assert_eq!(
        reorder(&s(&["--device", "1", "status"])),
        s(&["--device", "1", "status"])
    );
    assert_eq!(
        reorder(&s(&["status", "--device"])),
        s(&["status", "--device"])
    );
}

#[test]
fn test_cli_raw_needs_an_obsbot_address_and_the_app() {
    let usb = FakeUsb::new();
    let mut out: Vec<String> = Vec::new();
    let make = maker(&usb, 1, false);
    assert_eq!(
        cli(
            &s(&["raw", "/other/thing"]),
            &mut |l| out.push(l.into()),
            &make
        ),
        1
    );
    assert_eq!(out.last().unwrap(), "The address starts with /OBSBOT/.");
    assert_eq!(
        cli(
            &s(&["raw", "/OBSBOT/WebCam/General/GetZoomInfo", "0"]),
            &mut |l| out.push(l.into()),
            &make
        ),
        1
    );
    assert_eq!(out.last().unwrap(), APP_NEEDED_LINE);
}

#[test]
fn test_cli_raw_lists_what_came_back() {
    let usb = FakeUsb::new();
    let center = FakeCenter::new(Some(usb.clone()));
    let mut out: Vec<String> = Vec::new();
    let make = maker(&usb, center.port, true);
    let argv = s(&["raw", "/OBSBOT/WebCam/General/GetZoomInfo", "0", "--json"]);
    assert_eq!(cli(&argv, &mut |l| out.push(l.into()), &make), 0);
    let v: Value = serde_json::from_str(out.last().unwrap()).unwrap();
    assert_eq!(
        v["replies"],
        json!([["/OBSBOT/WebCam/General/ZoomInfoResp", [25, -1.0]]])
    );
    assert_eq!(v["line"], "Sent /OBSBOT/WebCam/General/GetZoomInfo.\n  /OBSBOT/WebCam/General/ZoomInfoResp [25, -1.0]");
    assert!(center.arrives("/OBSBOT/WebCam/General/GetZoomInfo", vec![0.into()]));
    assert_eq!(number("12"), OscValue::Int(12));
    assert_eq!(number("-1.5"), OscValue::Float(-1.5));
    assert_eq!(number("1_000"), OscValue::Int(1000));
    assert_eq!(number("Desk"), OscValue::Str("Desk".into()));
}

#[test]
fn test_setup_reads_the_camera_and_turns_osc_on_while_the_app_is_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let ini = tmp.path().join("global.ini");
    std::fs::write(
        &ini,
        "[SoftSetting]\nOSC=false\nOSCHostIp=\nOSCReceivePort=16284\n",
    )
    .unwrap();
    let usb = FakeUsb::new();
    let mut out: Vec<String> = Vec::new();
    assert_eq!(
        setup_with(
            &ini,
            &mut |l| out.push(l.into()),
            Some(Box::new(usb)),
            tmp.path(),
            &|| false
        ),
        0
    );
    let text = std::fs::read_to_string(&ini).unwrap();
    assert!(
        text.contains("OSC=true")
            && text.contains("OSCHostIp=127.0.0.1")
            && text.contains("OSCConnectMethod=0")
    );
    assert_eq!(
        text,
        "[SoftSetting]\nOSCConnectMethod=0\nOSC=true\nOSCHostIp=127.0.0.1\nOSCReceivePort=16284\n"
    );
    assert!(out[0].contains("answers over USB"));
}

#[test]
fn test_setup_adds_the_section_when_it_is_missing() {
    let tmp = tempfile::tempdir().unwrap();
    let ini = tmp.path().join("global.ini");
    std::fs::write(&ini, "[Other]\nx=1").unwrap();
    let mut out: Vec<String> = Vec::new();
    assert_eq!(
        setup_with(
            &ini,
            &mut |l| out.push(l.into()),
            Some(Box::new(FakeUsb::new())),
            tmp.path(),
            &|| false
        ),
        0
    );
    assert_eq!(
        std::fs::read_to_string(&ini).unwrap(),
        "[Other]\nx=1\n[SoftSetting]\nOSC=true\nOSCConnectMethod=0\nOSCHostIp=127.0.0.1\nOSCReceivePort=16284\n"
    );
    assert_eq!(out.last().unwrap(), "OSC is on in OBSBOT Center's settings: open the app when you want tracking or AI modes from eve.");
}

#[test]
fn test_setup_leaves_a_running_app_alone() {
    let tmp = tempfile::tempdir().unwrap();
    let ini = tmp.path().join("global.ini");
    std::fs::write(&ini, "[SoftSetting]\nOSC=false\n").unwrap();
    let mut out: Vec<String> = Vec::new();
    assert_eq!(
        setup_with(
            &ini,
            &mut |l| out.push(l.into()),
            Some(Box::new(FakeUsb::new())),
            tmp.path(),
            &|| true
        ),
        0
    );
    assert_eq!(
        std::fs::read_to_string(&ini).unwrap(),
        "[SoftSetting]\nOSC=false\n"
    );
    assert!(out.last().unwrap().contains("turn on OSC in its settings"));
}

#[test]
fn test_setup_without_the_app_is_fine_for_moving() {
    let tmp = tempfile::tempdir().unwrap();
    let mut out: Vec<String> = Vec::new();
    let missing = tmp.path().join("no-app");
    let ini = tmp.path().join("global.ini");
    assert_eq!(
        setup_with(
            &ini,
            &mut |l| out.push(l.into()),
            Some(Box::new(FakeUsb::new())),
            &missing,
            &|| false
        ),
        0
    );
    assert!(out
        .last()
        .unwrap()
        .starts_with("OBSBOT Center isn't set up"));
}

#[test]
fn test_setup_without_a_camera_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let usb = FakeUsb::new();
    usb.0.lock().unplugged = true;
    let mut out: Vec<String> = Vec::new();
    assert_eq!(
        setup(
            &tmp.path().join("none.ini"),
            &mut |l| out.push(l.into()),
            Some(Box::new(usb))
        ),
        1
    );
    assert_eq!(out, vec![NO_CAMERA_LINE.to_string()]);
}

// --- test_obsbot_brain.py / test_obsbot_panel.py: what the panel reads from this crate ------------------------------

#[test]
fn test_brain_status_turn_point_zoom_center_reads() {
    let usb = FakeUsb::new();
    let mut c = cam(&usb);
    let st = c.status();
    assert!(
        ok(&st)
            && st["pitch"] == 0
            && st["camera"] == "Tiny 3"
            && st["awake"] == true
            && st["zoom"] == 0
    );
    // turn left 15: the panel reads the position, then moves to (15, 0)
    assert_eq!(
        Value::Object(c.position_reply()),
        json!({"ok": true, "pan": 0, "pitch": 0, "line": ""})
    );
    let res = c.move_to(Some(15.0), Some(0.0));
    assert!(ok(&res) && res["pan"] == 15 && res["pitch"] == 0);
    // point pan -30, tilt 15 (tilt + up): pitch -15, the camera's tilt 15
    c.move_to(Some(-30.0), Some(-15.0));
    assert!(usb.calls().contains(&Call::Point(-30.0, 15.0)));
    assert_eq!(
        Value::Object(c.zoom_reply()),
        json!({"ok": true, "zoom": 0, "line": ""})
    );
    assert_eq!(c.zoom(25)["zoom"], 25);
    assert_eq!(c.zoom(-25)["zoom"], 0); // clamped
    let res = c.reset();
    assert!(ok(&res) && res["pan"] == 0 && res["pitch"] == 0);
}

#[test]
fn test_brain_refusals() {
    let usb = FakeUsb::new();
    let mut c = cam(&usb);
    assert_eq!(line(&c.track(true)), APP_NEEDED_LINE);
    usb.0.lock().unplugged = true;
    let mut gone = cam(&usb);
    assert_eq!(gone.ensure().as_deref(), Some(NO_CAMERA_LINE));
    assert_eq!(line(&gone.position_reply()), NO_CAMERA_LINE);
    assert_eq!(line(&gone.zoom_reply()), "Couldn't read the zoom.");
}

#[test]
fn test_panel_constants() {
    assert_eq!(
        keys(&AI_MODES),
        ["single", "group", "voice", "desk", "hand", "whiteboard"]
    );
    assert_eq!(keys(&DIRECTIONS), ["up", "down", "left", "right"]);
    assert_eq!(APP_PATH, "/Applications/OBSBOT_Center.app");
    assert_eq!(PITCH_RANGE, (-59.0, 59.0));
}

// --- the Python behaviors the port leans on ---------------------------------------------------------------------

#[test]
fn test_python_helpers() {
    use py::*;
    assert_eq!(float_repr(25.0), "25.0");
    assert_eq!(float_repr(0.1), "0.1");
    assert_eq!(float_repr(1e16), "1e+16");
    assert_eq!(float_repr(1e15), "1000000000000000.0");
    assert_eq!(float_repr(0.0001), "0.0001");
    assert_eq!(float_repr(0.00001), "1e-05");
    assert_eq!(float_repr(-1.5e-7), "-1.5e-07");
    assert_eq!(float_repr(-0.0), "-0.0");
    assert_eq!(round_even(2.5), Some(2));
    assert_eq!(round_even(-3.5), Some(-4));
    assert_eq!(parse_int(" 1_0 "), Some(10));
    assert_eq!(parse_int("1__0"), None);
    assert_eq!(parse_int("1.5"), None);
    assert_eq!(parse_float("1e3"), Some(1000.0));
    assert!(parse_float("-NaN").is_some_and(f64::is_nan));
    assert_eq!(parse_float("0x10"), None);
    assert_eq!(splitlines("a\r\nb\rc\n"), ["a", "b", "c"]);
    assert_eq!(
        dumps(&json!({"a": [1, 2.0], "b": "é"})),
        "{\"a\": [1, 2.0], \"b\": \"é\"}"
    );
    assert_eq!(str_repr("a\nb"), "'a\\nb'");
    // repr escapes what str.isprintable() refuses, not only control characters
    assert_eq!(str_repr("a\u{a0}b"), "'a\\xa0b'");
    assert_eq!(str_repr("\u{200b}"), "'\\u200b'");
    assert_eq!(str_repr("caf\u{e9} \u{1f600}"), "'caf\u{e9} \u{1f600}'");
}

#[test]
fn test_raw_repr_of_values_python_cannot_put_in_json() {
    // the raw command's line is Python's repr of the decoded list: nan and inf print as such, not None
    assert_eq!(
        values_repr(&[OscValue::Float(f64::NAN), OscValue::Float(f64::INFINITY), OscValue::Float(f64::NEG_INFINITY)]),
        "[nan, inf, -inf]"
    );
    let data = encode("/x", &[OscValue::Float(f64::INFINITY)]).unwrap();
    assert_eq!(decode(&data).unwrap().1, vec![OscValue::Float(f64::INFINITY)]);
    assert_eq!(values_repr(&[]), "[]");
}
