//! tests/test_lights.py: colours, each protocol's bytes, which lights a request means, the code path, the tools.

use std::collections::{HashMap, HashSet};
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use serde_json::{json, Map, Value};
use sha2::Sha256;

use crate::govee::{govee_commands, GoveeDriver, GoveePorts};
use crate::lights::*;
use crate::py;
use crate::triones::*;
use crate::tuya::*;
use crate::wiz::*;

type Calls = Arc<parking_lot::Mutex<Vec<(String, Change)>>>;

struct FakeDriver {
    light: Light,
    log: Calls,
    fail: HashSet<String>,
}

#[async_trait]
impl Driver for FakeDriver {
    async fn state(&self) -> Result<State, LightError> {
        if self.fail.contains(&self.light.name) {
            return Err(LightError::Connection("unreachable".into()));
        }
        Ok(State { on: Some(true), brightness: Some(40), rgb: Some([0, 0, 255]), kelvin: None })
    }

    async fn apply(&self, change: &Change) -> Result<(), LightError> {
        if self.fail.contains(&self.light.name) {
            return Err(LightError::Connection("unreachable".into()));
        }
        self.log.lock().push((self.light.name.clone(), change.clone()));
        Ok(())
    }

    async fn close(&self) -> Result<(), LightError> {
        Ok(())
    }
}

fn fake_drivers(calls: &Calls, fail: &[&str]) -> HashMap<String, DriverFactory> {
    let fail: HashSet<String> = fail.iter().map(|s| s.to_string()).collect();
    KINDS
        .iter()
        .map(|k| {
            let (calls, fail) = (calls.clone(), fail.clone());
            let f: DriverFactory = Arc::new(move |lt: &Light| {
                Arc::new(FakeDriver { light: lt.clone(), log: calls.clone(), fail: fail.clone() }) as Arc<dyn Driver>
            });
            (k.to_string(), f)
        })
        .collect()
}

fn light(name: &str, kind: &str) -> Light {
    Light::new(name, kind)
}

fn hub_lights() -> Vec<Light> {
    vec![
        Light { room: "bedroom".into(), ip: "192.0.2.10".into(), device: "AA:BB".into(), ..light("floor lamp", "govee") },
        Light {
            room: "bedroom".into(),
            address: "uuid-1".into(),
            aliases: vec!["led strip".into()],
            ..light("strip", "triones")
        },
        Light { room: "office".into(), id: "abc".into(), key: "k".into(), ..light("desk bulb", "tuya") },
    ]
}

fn hub(fail: &[&str]) -> (Lights, Calls) {
    let calls: Calls = Arc::default();
    let h = Lights::with(hub_lights(), Some(fake_drivers(&calls, fail)), Duration::from_secs_f64(LIGHT_TIMEOUT_SECS));
    (h, calls)
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap()
}

fn pc(text: &str) -> Option<Color> {
    parse_color(text)
}

fn names(cmd: &Command) -> Vec<&str> {
    cmd.names.iter().map(String::as_str).collect()
}

// ---- colour ----

#[test]
fn test_parse_color_names_whites_hex_kelvin() {
    assert_eq!(pc("Blue").unwrap().rgb, Some([0, 0, 255]));
    assert_eq!(pc("warm white").unwrap().kelvin, Some(2700));
    assert_eq!(pc("daylight").unwrap().kelvin, Some(6500));
    assert_eq!(pc("#FF8800").unwrap().rgb, Some([255, 136, 0]));
    assert_eq!(pc("ff8800").unwrap().rgb, Some([255, 136, 0]));
    assert_eq!(pc("2700K").unwrap().kelvin, Some(2700));
    assert_eq!(pc("12000k").unwrap().kelvin, Some(KELVIN_MAX));
    assert_eq!(pc("red light").unwrap().rgb, Some([255, 0, 0]));
    assert!(pc("plaid").is_none());
}

#[test]
fn test_kelvin_to_rgb_is_warm_to_cool() {
    let (warm, cool) = (kelvin_to_rgb(2700), kelvin_to_rgb(6500));
    assert!(warm[0] == 255 && warm[2] < warm[1] && warm[1] < 255);
    assert!(cool[2] >= 250 && (cool[0] - cool[2]).abs() < 20);
}

#[test]
fn kelvin_to_rgb_matches_python_values() {
    // values the Python function returns (round half to even)
    assert_eq!(kelvin_to_rgb(2700), [255, 167, 87]);
    assert_eq!(kelvin_to_rgb(6500), [255, 254, 250]);
    assert_eq!(kelvin_to_rgb(4000), [255, 206, 166]);
    assert_eq!(kelvin_to_rgb(9000), [210, 223, 255]);
}

#[test]
fn test_scale_rgb_keeps_hue_and_sets_level() {
    assert_eq!(scale_rgb(&[0, 0, 128], 100), [0, 0, 255]);
    assert_eq!(scale_rgb(&[255, 128, 0], 50), [128, 64, 0]);
    assert_eq!(scale_rgb(&[0, 0, 0], 50), [128, 128, 128]);
    assert_eq!(rgb_percent(&[0, 0, 128]), 50);
}

#[test]
fn test_describe_names_a_picked_colour() {
    assert_eq!(describe(&Change { color: pc("#19ffff"), brightness: Some(50), ..Change::default() }), "cyan (#19ffff) at 50%");
    assert_eq!(describe(&Change { color: pc("blue"), ..Change::default() }), "blue");
}

#[test]
fn test_color_word() {
    assert_eq!(color_word(&[224, 0, 0]), "red");
    assert_eq!(color_word(&[0, 0, 200]), "blue");
    assert_eq!(color_word(&[250, 250, 245]), "white");
}

// ---- Govee ----

#[test]
fn test_govee_off_is_one_turn() {
    let off = Change { power: Some(false), ..Change::default() };
    assert_eq!(govee_commands(&off, None), vec![json!({"msg": {"cmd": "turn", "data": {"value": 0}}})]);
}

#[test]
fn test_govee_colour_and_level_turn_on_first() {
    let msgs = govee_commands(&Change { brightness: Some(40), color: pc("blue"), ..Change::default() }, None);
    let cmds: Vec<&str> = msgs.iter().map(|m| m["msg"]["cmd"].as_str().unwrap()).collect();
    assert_eq!(cmds, ["turn", "colorwc", "brightness"]);
    assert_eq!(msgs[0]["msg"]["data"], json!({"value": 1}));
    assert_eq!(msgs[1]["msg"]["data"], json!({"color": {"r": 0, "g": 0, "b": 255}, "colorTemInKelvin": 0}));
    assert_eq!(msgs[2]["msg"]["data"], json!({"value": 40}));
    // the bytes on the wire are Python's json.dumps: spaces after , and :
    assert_eq!(py::dumps(&msgs[0]), r#"{"msg": {"cmd": "turn", "data": {"value": 1}}}"#);
}

#[test]
fn test_govee_white_uses_kelvin_clamped() {
    let c = Change { color: Some(Color { kelvin: Some(1500), word: "x".into(), rgb: None }), ..Change::default() };
    let msgs = govee_commands(&c, None);
    assert_eq!(msgs[1]["msg"]["data"]["colorTemInKelvin"], json!(GOVEE_KELVIN_MIN));
}
const GOVEE_KELVIN_MIN: i64 = crate::govee::GOVEE_KELVIN.0;

#[test]
fn test_govee_brightness_zero_means_off() {
    assert_eq!(
        govee_commands(&Change { brightness: Some(0), ..Change::default() }, None),
        vec![json!({"msg": {"cmd": "turn", "data": {"value": 0}}})]
    );
}

// ---- WiZ ----

fn obj(v: Value) -> Map<String, Value> {
    v.as_object().unwrap().clone()
}

#[test]
fn test_wiz_off_is_setstate_false() {
    assert_eq!(wiz_params(&Change { power: Some(false), ..Change::default() }), obj(json!({"state": false})));
    assert_eq!(wiz_params(&Change { brightness: Some(0), ..Change::default() }), obj(json!({"state": false})));
}

#[test]
fn test_wiz_colour_and_level_in_one_pilot() {
    let got = wiz_params(&Change { brightness: Some(40), color: pc("blue"), ..Change::default() });
    assert_eq!(Value::Object(got), json!({"state": true, "r": 0, "g": 0, "b": 255, "dimming": 40}));
}

#[test]
fn test_wiz_white_is_temp_clamped_and_dimming_floored() {
    let c = Change {
        color: Some(Color { kelvin: Some(1500), word: "x".into(), rgb: None }),
        brightness: Some(3),
        ..Change::default()
    };
    assert_eq!(Value::Object(wiz_params(&c)), json!({"state": true, "temp": WIZ_KELVIN.0, "dimming": WIZ_MIN_DIMMING}));
}

#[test]
fn test_wiz_parse_pilot() {
    let s = wiz_parse_pilot(&obj(json!({"mac": "x", "state": true, "r": 255, "g": 0, "b": 0, "dimming": 60}))).unwrap();
    assert_eq!((s.on, s.brightness, s.rgb, s.kelvin), (Some(true), Some(60), Some([255, 0, 0]), None));
    let w = wiz_parse_pilot(&obj(json!({"state": false, "temp": 2700, "dimming": 100}))).unwrap();
    assert_eq!((w.on, w.kelvin, w.rgb), (Some(false), Some(2700), None));
    let scene = wiz_parse_pilot(&obj(json!({"state": true, "sceneId": 4, "dimming": 50}))).unwrap();
    assert!(scene.rgb.is_none() && scene.kelvin.is_none());
}

/// A UDP 'bulb' on localhost that records each request and answers it with `reply`, once.
fn fake_wiz(reply: Value) -> (u16, Arc<parking_lot::Mutex<Vec<Value>>>) {
    let srv = UdpSocket::bind("127.0.0.1:0").unwrap();
    srv.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let port = srv.local_addr().unwrap().port();
    let got: Arc<parking_lot::Mutex<Vec<Value>>> = Arc::default();
    let seen = got.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let Ok((n, addr)) = srv.recv_from(&mut buf) else { return };
        let req: Value = serde_json::from_slice(&buf[..n]).unwrap();
        let mut out = Map::new();
        out.insert("method".into(), req["method"].clone());
        out.extend(reply.as_object().unwrap().clone());
        seen.lock().push(req);
        let _ = srv.send_to(py::dumps(&Value::Object(out)).as_bytes(), addr);
    });
    (port, got)
}

#[test]
fn test_wiz_driver_sends_setpilot_and_reads_reply() {
    let (port, got) = fake_wiz(json!({"result": {"success": true}}));
    let d = WizDriver::with_port(Light { ip: "127.0.0.1".into(), device: "aabbccddeeff".into(), ..light("bulb", "wiz") }, port);
    rt().block_on(d.apply(&Change { color: pc("red"), ..Change::default() })).unwrap();
    assert_eq!(*got.lock(), vec![json!({"id": 1, "method": "setPilot", "params": {"state": true, "r": 255, "g": 0, "b": 0}})]);
}

#[test]
fn test_wiz_error_reply_raises() {
    let (port, _) = fake_wiz(json!({"error": {"code": -32600, "message": "Invalid Request"}}));
    let err = wiz_call_at("127.0.0.1", port, "getPilot", &Map::new(), 1, 0.7, "", None).unwrap_err();
    assert!(matches!(err, LightError::Connection(_)), "{err:?}");
    assert_eq!(err.to_string(), "the WiZ light refused getPilot: Invalid Request");
}

const KEY: &str = "00112233445566778899aabbccddeeff"; // a made-up key, not a real home's

fn good_hmac(params: &Value) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(&py::fromhex(KEY).unwrap()).unwrap();
    mac.update(py::dumps_compact(params).as_bytes());
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// A UDP 'bulb' that answers reads, and accepts a change only with a valid hmac over its params (and, when
/// clock_ok is false, only with sigTs == bulb_ts + 1): firmware 1.38's behaviour.
fn signing_bulb(bulb_ts: i64, clock_ok: bool) -> (u16, Arc<parking_lot::Mutex<Vec<Value>>>) {
    let srv = UdpSocket::bind("127.0.0.1:0").unwrap();
    srv.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
    let port = srv.local_addr().unwrap().port();
    let got: Arc<parking_lot::Mutex<Vec<Value>>> = Arc::default();
    let seen = got.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        while let Ok((n, addr)) = srv.recv_from(&mut buf) {
            let req: Value = serde_json::from_slice(&buf[..n]).unwrap();
            seen.lock().push(req.clone());
            let out = if req["method"] == "getPilot" {
                json!({"method": "getPilot", "result": {"state": true, "dimming": 50, "sigTs": bulb_ts}})
            } else {
                let ok = req.get("hmac") == Some(&json!(good_hmac(&req["params"])))
                    && (clock_ok || req["params"]["sigTs"] == json!(bulb_ts + 1));
                if ok {
                    json!({"method": req["method"], "result": {"success": true}})
                } else {
                    json!({"method": req["method"], "error": {"code": -32602, "message": "Invalid params"}})
                }
            };
            let _ = srv.send_to(py::dumps(&out).as_bytes(), addr);
        }
    });
    (port, got)
}

#[test]
fn test_wiz_sign_is_hmac_sha256_of_compact_params() {
    let params = obj(json!({"state": true, "r": 0, "g": 255, "b": 0, "sigTs": 5}));
    let mut mac = Hmac::<Sha256>::new_from_slice(&py::fromhex(KEY).unwrap()).unwrap();
    mac.update(br#"{"state":true,"r":0,"g":255,"b":0,"sigTs":5}"#);
    let want = mac.finalize().into_bytes().to_vec();
    let got = base64::engine::general_purpose::STANDARD.decode(wiz_sign(KEY, &params).unwrap()).unwrap();
    assert_eq!(got, want);
    let msg: Value = serde_json::from_slice(&wiz_message("setPilot", &obj(json!({"state": false})), KEY, Some(7)).unwrap()).unwrap();
    assert_eq!(msg["params"], json!({"state": false, "sigTs": 7}));
    assert_eq!(msg["hmac"], json!(wiz_sign(KEY, &obj(json!({"state": false, "sigTs": 7}))).unwrap()));
    let read: Value = serde_json::from_slice(&wiz_message("getPilot", &Map::new(), KEY, None).unwrap()).unwrap();
    assert!(read.get("hmac").is_none()); // reads go unsigned
    let bare: Value = serde_json::from_slice(&wiz_message("setPilot", &obj(json!({"state": true})), "", None).unwrap()).unwrap();
    assert!(bare.get("hmac").is_none()); // no key: unsigned
    // the exact bytes: compact, in this key order
    assert_eq!(
        String::from_utf8(wiz_message("getPilot", &Map::new(), "", None).unwrap()).unwrap(),
        r#"{"id":1,"method":"getPilot","params":{}}"#
    );
}

#[test]
fn test_a_signed_change_is_accepted_and_an_unsigned_one_names_the_fix() {
    let (port, got) = signing_bulb(1000, true);
    let rt = rt();
    let d = WizDriver::with_port(
        Light { ip: "127.0.0.1".into(), device: "aabb".into(), key: KEY.into(), ..light("wiz", "wiz") },
        port,
    );
    rt.block_on(d.apply(&Change { color: pc("green"), ..Change::default() })).unwrap();
    {
        let got = got.lock();
        let last = got.last().unwrap();
        assert!(last["method"] == "setPilot" && last.get("hmac").is_some());
    }
    let bare = WizDriver::with_port(Light { ip: "127.0.0.1".into(), device: "aabb".into(), ..light("wiz", "wiz") }, port);
    let err = rt.block_on(bare.apply(&Change { power: Some(false), ..Change::default() })).unwrap_err();
    assert!(matches!(err, LightError::Refused(_)) && err.to_string().contains("wiz-import"), "{err:?}");
}

#[test]
fn test_a_clock_the_bulb_disagrees_with_is_retried_on_the_bulbs_clock() {
    let (port, got) = signing_bulb(1000, false);
    wiz_call_signed_at("127.0.0.1", port, "setPilot", &obj(json!({"state": true})), KEY).unwrap();
    let got = got.lock();
    let methods: Vec<&str> = got.iter().map(|r| r["method"].as_str().unwrap()).collect();
    assert_eq!(methods, ["setPilot", "getPilot", "setPilot"]);
    assert_eq!(got.last().unwrap()["params"]["sigTs"], json!(1001));
}

#[test]
fn test_wiz_import_takes_the_key_by_mac() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("lights.json");
    save(
        &[
            Light { ip: "192.0.2.5".into(), device: "aabbccddeeff".into(), ..light("wiz", "wiz") },
            Light { ip: "192.0.2.6".into(), ..light("lamp", "govee") },
        ],
        Some(&p),
    )
    .unwrap();
    let export = tmp.path().join("export.json");
    std::fs::write(&export, py::dumps(&json!({"udp_signing_key": KEY, "devices": [{"mac_address": "AABBCCDDEEFF"}]})))
        .unwrap();
    let mut said: Vec<String> = Vec::new();
    let args = vec!["wiz-import".to_string(), export.display().to_string()];
    assert_eq!(crate::cli(&args, Some(&p), &mut |l| said.push(l.to_string())), 0);
    let got: HashMap<String, Light> = load(Some(&p)).into_iter().map(|lt| (lt.name.clone(), lt)).collect();
    assert!(got["wiz"].key == KEY && got["lamp"].key.is_empty() && said.last().unwrap().contains("wiz"));
    assert_eq!(mode(&p), 0o600);
    std::fs::write(&export, py::dumps(&json!({"udp_signing_key": "not hex", "devices": []}))).unwrap();
    assert_eq!(crate::cli(&args, Some(&p), &mut |l| said.push(l.to_string())), 1);
    assert_eq!(said.last().unwrap(), &format!("Could not read a WiZ export from {}: ValueError", export.display()));
}

#[cfg(unix)]
fn mode(p: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).unwrap().permissions().mode() & 0o7777
}

#[test]
fn test_wiz_light_roundtrips_and_merges() {
    let lt = Light::from_dict(&json!({"name": "bulb", "kind": "wiz", "ip": "192.0.2.5", "device": "aabb", "sku": "ESP"}))
        .unwrap();
    assert_eq!(lt.to_dict(), json!({"name": "bulb", "kind": "wiz", "ip": "192.0.2.5", "device": "aabb", "sku": "ESP"}));
    let (merged, new) = crate::merge(
        vec![lt],
        vec![Light { ip: "192.0.2.9".into(), device: "aabb".into(), ..light("wiz aabb", "wiz") }],
    );
    assert!(new.is_empty() && merged[0].name == "bulb" && merged[0].ip == "192.0.2.9");
}

// ---- Triones ----

fn hex(s: &str) -> Vec<u8> {
    py::fromhex(s).unwrap()
}

#[test]
fn test_triones_frames_bytes() {
    assert!(TRIONES_ON.to_vec() == hex("cc2333") && TRIONES_OFF.to_vec() == hex("cc2433"));
    assert_eq!(triones_color(&[1, 2, 3]), hex("5601020300f0aa"));
    assert_eq!(triones_frames(&Change { power: Some(false), ..Change::default() }, None), vec![TRIONES_OFF.to_vec()]);
}

#[test]
fn test_triones_status_parse() {
    let st = triones_parse_status(&hex("66e3234120017b0000000399")).unwrap();
    assert!(st.on == Some(true) && st.rgb == Some([123, 0, 0]) && st.brightness == Some(48));
    let off = triones_parse_status(&hex("66e3244120017b0000000399")).unwrap();
    assert_eq!(off.on, Some(false));
    assert!(triones_parse_status(&[0x66, 0x01]).is_none());
}

#[test]
fn test_triones_colour_keeps_current_level() {
    let cur = State { on: Some(true), rgb: Some([128, 0, 0]), brightness: Some(50), kelvin: None };
    let frames = triones_frames(&Change { color: pc("blue"), ..Change::default() }, Some(&cur));
    assert_eq!(frames, vec![TRIONES_ON.to_vec(), triones_color(&[0, 0, 128])]);
}

#[test]
fn test_triones_level_keeps_current_colour() {
    let cur = State { on: Some(true), rgb: Some([255, 0, 0]), brightness: Some(100), kelvin: None };
    let frames = triones_frames(&Change { brightness: Some(20), ..Change::default() }, Some(&cur));
    assert_eq!(frames.last().unwrap(), &triones_color(&[51, 0, 0]));
}

#[test]
fn test_triones_colour_on_an_off_light_is_full() {
    let cur = State { on: Some(false), rgb: Some([10, 0, 0]), ..State::default() };
    let frames = triones_frames(&Change { color: pc("green"), ..Change::default() }, Some(&cur));
    assert_eq!(frames.last().unwrap(), &triones_color(&[0, 255, 0]));
}

// ---- Tuya ----

#[test]
fn test_tuya_temp_percent() {
    assert!(tuya_temp_percent(2700) == 0 && tuya_temp_percent(6500) == 100);
    assert!(tuya_temp_percent(1000) == 0 && tuya_temp_percent(4000) == 34);
}

#[test]
fn test_tuya_without_key_says_so() {
    let d = TuyaDriver::new(Light { id: "x".into(), ..light("b", "tuya") });
    let err = rt().block_on(d.apply(&Change { power: Some(true), ..Change::default() })).unwrap_err();
    assert!(matches!(err, LightError::Permission(_)), "{err:?}");
}

#[test]
fn tuya_calls_are_the_bulbdevice_calls() {
    assert_eq!(tuya_calls(&Change { power: Some(false), ..Change::default() }), vec![json!(["turn_off"])]);
    assert_eq!(
        tuya_calls(&Change { brightness: Some(30), color: pc("warm white"), ..Change::default() }),
        vec![json!(["turn_on"]), json!(["set_white_percentage", 30, 0])]
    );
    assert_eq!(
        tuya_calls(&Change { brightness: Some(40), color: pc("blue"), ..Change::default() }),
        vec![json!(["turn_on"]), json!(["set_colour", 0, 0, 255]), json!(["set_brightness_percentage", 40])]
    );
    assert_eq!(
        tuya_calls(&Change { color: pc("daylight"), ..Change::default() }),
        vec![json!(["turn_on"]), json!(["set_white_percentage", 100, 100])]
    );
}

// ---- the file ----

#[test]
fn test_load_save_roundtrip_is_private() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("lights.json");
    let (h, _) = hub(&[]);
    save(&h.lights, Some(&p)).unwrap();
    assert_eq!(mode(&p), 0o600);
    let back = load(Some(&p));
    let dicts = |ls: &[Light]| ls.iter().map(Light::to_dict).collect::<Vec<_>>();
    assert_eq!(dicts(&back), dicts(&h.lights));
    let first = back[0].to_dict();
    assert!(first.get("address").is_none() && first.get("key").is_none());
}

#[test]
fn save_writes_what_python_writes() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("lights.json");
    save(&[Light { room: "s\u{e9}jour".into(), ip: "192.0.2.1".into(), ..light("a", "govee") }], Some(&p)).unwrap();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "{\n  \"lights\": [\n    {\n      \"name\": \"a\",\n      \"kind\": \"govee\",\n      \"room\": \"s\\u00e9jour\",\n      \"ip\": \"192.0.2.1\"\n    }\n  ]\n}\n"
    );
    save(&[], Some(&p)).unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "{\n  \"lights\": []\n}\n");
}

#[test]
fn test_load_skips_bad_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("lights.json");
    std::fs::write(&p, py::dumps(&json!({"lights": [{"name": "x", "kind": "hue"}, {"kind": "govee"}, {"name": "ok", "kind": "govee"}]})))
        .unwrap();
    let got: Vec<String> = load(Some(&p)).into_iter().map(|lt| lt.name).collect();
    assert_eq!(got, ["ok"]);
    assert!(load(Some(&tmp.path().join("missing.json"))).is_empty());
    std::fs::write(&p, "not json").unwrap();
    assert!(load(Some(&p)).is_empty());
}

#[test]
fn from_dict_reads_like_python() {
    let lt = Light::from_dict(&json!({"name": " Lamp ", "kind": " GOVEE ", "aliases": ["a", " ", 5], "version": 3.4})).unwrap();
    assert_eq!((lt.name.as_str(), lt.kind.as_str()), ("Lamp", "govee"));
    assert_eq!(lt.aliases, ["a", "5"]);
    assert_eq!(lt.version, "3.4");
    assert_eq!(Light::from_dict(&json!({"name": "x", "kind": "hue"})).unwrap_err(), "unknown light kind 'hue'");
    assert_eq!(Light::from_dict(&json!({"kind": "wiz"})).unwrap_err(), "a light needs a name");
}

#[test]
fn test_merge_keeps_names_and_takes_new_address() {
    let known = vec![Light { room: "bedroom".into(), ip: "192.0.2.10".into(), device: "AA:BB".into(), ..light("floor lamp", "govee") }];
    let found = vec![
        Light { ip: "192.0.2.99".into(), device: "AA:BB".into(), ..light("govee h6076", "govee") },
        Light { address: "u2".into(), ..light("floor lamp", "triones") },
    ];
    let (merged, new) = crate::merge(known, found);
    assert!(merged[0].name == "floor lamp" && merged[0].ip == "192.0.2.99" && merged[0].room == "bedroom");
    assert_eq!(new.iter().map(|n| n.name.as_str()).collect::<Vec<_>>(), ["floor lamp 2"]);
}

#[test]
fn test_make_lights_off_switch_and_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("lights.json");
    let empty = HashMap::new();
    assert!(make_lights(Some(&p), Some(&empty)).is_none());
    save(&[light("a", "govee")], Some(&p)).unwrap();
    let off: HashMap<String, String> = [("EVE_LIGHTS".to_string(), "0".to_string())].into();
    assert!(make_lights(Some(&p), Some(&off)).is_none());
    assert!(make_lights(Some(&p), Some(&empty)).is_some());
    assert!(!enabled(Some(&[("EVE_LIGHTS".to_string(), " Off ".to_string())].into())));
    assert!(enabled(Some(&[("EVE_LIGHTS".to_string(), "1".to_string())].into())));
}

// ---- which lights ----

#[test]
fn test_resolve() {
    let (h, _) = hub(&[]);
    let names = |t: &str| h.resolve(t).0.iter().map(|lt| lt.name.clone()).collect::<Vec<_>>();
    assert!(names("all") == names("the lights") && names("all") == names(""));
    assert_eq!(names("all"), ["floor lamp", "strip", "desk bulb"]);
    assert_eq!(names("Floor Lamp"), ["floor lamp"]);
    assert!(names("bedroom") == names("bedroom lights") && names("bedroom") == ["floor lamp", "strip"]);
    assert_eq!(names("lamp"), ["floor lamp"]);
    assert_eq!(names("led strip"), ["strip"]);
    assert!(names("govee") == ["floor lamp"] && names("sylvania") == ["desk bulb"]);
    let (got, why) = h.resolve("garage");
    assert!(got.is_empty() && why.contains("floor lamp"));
    assert_eq!(why, "no light called 'garage'; the lights are: floor lamp (bedroom), strip (bedroom), desk bulb (office)");
}

// ---- the code path ----

fn change(power: Option<bool>, brightness: Option<i64>, color: Option<&str>) -> Change {
    Change { power, brightness, color: color.and_then(parse_color) }
}

#[test]
fn test_match() {
    let all = ["floor lamp", "strip", "desk bulb"];
    let cases: Vec<(&str, Vec<&str>, Change)> = vec![
        ("lights off", all.to_vec(), change(Some(false), None, None)),
        ("Turn off the lights", all.to_vec(), change(Some(false), None, None)),
        ("turn the lights on please", all.to_vec(), change(Some(true), None, None)),
        ("lights out", all.to_vec(), change(Some(false), None, None)),
        ("lights blue", all.to_vec(), change(None, None, Some("blue"))),
        ("set all the lights to purple", all.to_vec(), change(None, None, Some("purple"))),
        ("make the lights warm white", all.to_vec(), change(None, None, Some("warm white"))),
        ("lights to 2700K", all.to_vec(), change(None, None, Some("2700k"))),
        ("lights #ff8800", all.to_vec(), change(None, None, Some("#ff8800"))),
        ("floor lamp red", vec!["floor lamp"], change(None, None, Some("red"))),
        ("dim the bedroom lights", vec!["floor lamp", "strip"], change(None, Some(DIM_PERCENT), None)),
        ("bedroom lights to 30%", vec!["floor lamp", "strip"], change(None, Some(30), None)),
        ("set the lamp to blue at 40%", vec!["floor lamp"], change(None, Some(40), Some("blue"))),
        ("turn off the lamp and the desk bulb", vec!["floor lamp", "desk bulb"], change(Some(false), None, None)),
        ("led strip pink", vec!["strip"], change(None, None, Some("pink"))),
    ];
    let (h, _) = hub(&[]);
    for (text, want, ch) in cases {
        let cmd = h.match_command(text).unwrap_or_else(|| panic!("{text}"));
        assert_eq!(cmd.change, ch, "{text}");
        assert_eq!(names(&cmd), want, "{text}");
    }
}

#[test]
fn test_match_leaves_the_rest_to_the_model() {
    let (h, _) = hub(&[]);
    for text in [
        "turn off the lights in ten minutes", "are the lights on", "why are the lights blue", "blue", "make it cozy",
        "lights", "turn on the tv", "what's the weather", "lights red and blue", "lights on off", "open spotify", "",
    ] {
        assert!(h.match_command(text).is_none(), "{text}");
    }
}

#[test]
fn test_match_without_lights_is_none() {
    assert!(Lights::new(vec![]).match_command("lights off").is_none());
}

#[test]
fn test_run_by_code_reports_each() {
    let rt = rt();
    let (h, calls) = hub(&[]);
    let line = rt.block_on(h.run(&h.match_command("lights off").unwrap()));
    assert!(line == "Lights off." && calls.lock().len() == 3);
    let (h2, _) = hub(&["strip"]);
    let line2 = rt.block_on(h2.run(&h2.match_command("lights blue at 50%").unwrap()));
    assert_eq!(line2, "Floor lamp and desk bulb blue at 50%. strip: unreachable.");
}

// ---- the tools ----

#[test]
fn test_tools_are_strict_and_complete() {
    let (h, _) = hub(&[]);
    for t in h.tools() {
        let params = &t["parameters"];
        assert!(t["strict"] == json!(true) && params["additionalProperties"] == json!(false));
        let mut required: Vec<&str> = params["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
        let mut props: Vec<&str> = params["properties"].as_object().unwrap().keys().map(String::as_str).collect();
        required.sort();
        props.sort();
        assert_eq!(required, props);
    }
    let names: HashSet<&str> = TOOL_NAMES.iter().copied().collect();
    assert_eq!(names, ["lights_set", "lights_match", "lights_status"].into_iter().collect());
    assert!(h.instructions().contains("floor lamp (room: bedroom)") && h.instructions().contains("led strip"));
}

#[test]
fn tools_and_instructions_are_byte_for_byte_python() {
    // json.dumps(lights.TOOLS[2]) and the instructions for one light, as the Python module writes them
    assert_eq!(
        py::dumps(&TOOLS[2]),
        r#"{"type": "function", "name": "lights_status", "strict": true, "description": "Whether the owner's lights are on, and their brightness and colour.", "parameters": {"type": "object", "additionalProperties": false, "required": ["target"], "properties": {"target": {"type": "string", "description": "\"all\", a light's name, or a room."}}}}"#
    );
    let h = Lights::new(vec![Light { room: "office".into(), aliases: vec!["lamp".into(), "desk".into()], ..light("desk lamp", "govee") }]);
    assert_eq!(
        h.instructions(),
        "You can control the owner's lights with lights_set (on, off, brightness, colour, white),\nmake lights match \
         another light with lights_match, and read them with lights_status. For a mood, pick the settings yourself: \
         cozy is warm white around 30%, reading is\nwhite at 100%, a movie is dim, a party is a colour. Say what you \
         did in a few words.\nThe lights:\n- desk lamp (room: office), also called lamp, desk"
    );
}

#[test]
fn test_handle_set_and_status() {
    let rt = rt();
    let (h, calls) = hub(&["desk bulb"]);
    let out = rt.block_on(h.handle("lights_set", &json!({"target": "all", "power": null, "brightness": 30, "color": "warm white"})));
    assert!(out["ok"] == json!(true) && out["done"] == json!(["floor lamp", "strip"]) && out["failed"][0]["name"] == "desk bulb");
    assert_eq!(calls.lock()[0].1, change(None, Some(30), Some("warm white")));
    let bad = rt.block_on(h.handle("lights_set", &json!({"target": "all", "power": null, "brightness": null, "color": "plaid"})));
    assert!(bad["ok"] == json!(false) && bad["reason"].as_str().unwrap().contains("plaid"));
    assert_eq!(
        bad["reason"],
        json!("unknown colour 'plaid': use a colour name, #rrggbb, a white or a temperature like 2700K")
    );
    let none = rt.block_on(h.handle("lights_set", &json!({"target": "all", "power": null, "brightness": null, "color": null})));
    assert_eq!(none["ok"], json!(false));
    let st = rt.block_on(h.handle("lights_status", &json!({"target": "bedroom"})));
    let names: Vec<&str> = st["lights"].as_array().unwrap().iter().map(|x| x["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["floor lamp", "strip"]);
    assert_eq!(
        st["lights"][0],
        json!({"name": "floor lamp", "room": "bedroom", "reachable": true, "on": true, "brightness": 40, "color": "blue", "rgb": "#0000ff"})
    );
    // and in that key order, as the model sees it
    assert_eq!(
        py::dumps(&st["lights"][0]),
        r##"{"name": "floor lamp", "room": "bedroom", "reachable": true, "on": true, "brightness": 40, "color": "blue", "rgb": "#0000ff"}"##
    );
    let st2 = rt.block_on(h.handle("lights_status", &json!({"target": "office"})));
    assert_eq!(st2["lights"][0]["reachable"], json!(false));
    assert_eq!(st2["lights"][0]["reason"], json!("unreachable"));
    let unknown = rt.block_on(h.handle("lights_dance", &json!({})));
    assert_eq!(unknown, json!({"ok": false, "reason": "unknown tool lights_dance"}));
}

// ---- matching one light to the others ----

#[test]
fn test_change_of_state() {
    assert_eq!(
        change_of(&State { on: Some(false), brightness: Some(50), rgb: Some([255, 0, 0]), kelvin: None }),
        change(Some(false), None, None)
    );
    let c = change_of(&State { on: Some(true), brightness: Some(40), rgb: Some([0, 0, 102]), kelvin: None });
    let col = c.color.clone().unwrap();
    assert!(c.power == Some(true) && c.brightness == Some(40) && col.rgb == Some([0, 0, 255]) && col.word == "blue");
    let w = change_of(&State { on: Some(true), brightness: Some(80), kelvin: Some(2700), rgb: Some([0, 0, 0]) });
    assert_eq!(w.color, Some(Color { kelvin: Some(2700), word: "2700K".into(), rgb: None }));
}

#[test]
fn test_match_copy() {
    let all = vec!["floor lamp", "strip", "desk bulb"];
    let cases: Vec<(&str, Vec<&str>, &str)> = vec![
        ("match the lights", all.clone(), ""),
        ("sync lights", all.clone(), ""),
        ("desk bulb same as the others", vec!["desk bulb"], "the others"),
        ("make the desk bulb the same light as the other lights rn", vec!["desk bulb"], "the other lights"),
        ("make the strip match the lamp", vec!["strip"], "the lamp"),
        ("match the desk bulb to the floor lamp please", vec!["desk bulb"], "the floor lamp"),
        ("match the desk bulb", vec!["desk bulb"], ""),
    ];
    let (h, _) = hub(&[]);
    for (text, want, source) in cases {
        let cmd = h.match_command(text).unwrap_or_else(|| panic!("{text}"));
        assert_eq!(cmd.source.as_deref(), Some(source), "{text}");
        assert_eq!(names(&cmd), want, "{text}");
    }
}

#[test]
fn test_match_copy_needs_real_lights() {
    let (h, _) = hub(&[]);
    for text in ["make the lights like a sunset", "match the garage to the lamp", "the strip is the same as yesterday"] {
        let cmd = h.match_command(text);
        assert!(cmd.as_ref().map(|c| c.source.is_none()).unwrap_or(true), "{text}");
    }
}

#[test]
fn test_copy_reads_a_wifi_light_first_and_sets_the_rest() {
    let (h, calls) = hub(&[]);
    let out = rt().block_on(h.copy("all", None));
    assert!(out["ok"] == json!(true) && out["source"] == "floor lamp" && out["done"] == json!(["strip", "desk bulb"]));
    assert_eq!(
        calls.lock()[0].1,
        Change { power: Some(true), brightness: Some(40), color: Some(Color { rgb: Some([0, 0, 255]), kelvin: None, word: "blue".into() }) }
    );
    assert_eq!(out["line"], json!("Strip and desk bulb matched to floor lamp: blue at 40%."));
}

#[test]
fn test_copy_skips_a_source_that_does_not_answer() {
    let (h, calls) = hub(&["floor lamp"]);
    let out = rt().block_on(h.copy("strip", None));
    assert_eq!(out["source"], json!("desk bulb"));
    assert_eq!(calls.lock().iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["strip"]);
}

#[test]
fn copy_with_no_answer_says_who_was_tried() {
    let (h, _) = hub(&["floor lamp", "desk bulb", "strip"]);
    let out = rt().block_on(h.copy("strip", None));
    assert_eq!(
        out,
        json!({"ok": false, "reason": "no light to copy answered (floor lamp: unreachable; desk bulb: unreachable)"})
    );
    let single = Lights::with(vec![light("a", "govee")], Some(fake_drivers(&Calls::default(), &[])), Duration::from_secs(1));
    let out = rt().block_on(single.copy("all", None));
    assert_eq!(out, json!({"ok": false, "reason": "a is the only light: nothing to match it to"}));
}

#[test]
fn test_copy_by_code_and_by_tool() {
    let rt = rt();
    let (h, calls) = hub(&[]);
    let line = rt.block_on(h.run(&h.match_command("make the strip match the desk bulb").unwrap()));
    assert_eq!(line, "Strip matched to desk bulb: blue at 40%.");
    assert_eq!(calls.lock().iter().map(|c| c.0.as_str()).collect::<Vec<_>>(), ["strip"]);
    let out = rt.block_on(h.handle("lights_match", &json!({"target": "desk bulb", "source": null})));
    assert!(out["ok"] == json!(true) && out["source"] == "floor lamp" && out.get("line").is_none());
    let bad = rt.block_on(h.handle("lights_match", &json!({"target": "garage", "source": null})));
    assert_eq!(bad["ok"], json!(false));
}

struct Slow;

#[async_trait]
impl Driver for Slow {
    async fn state(&self) -> Result<State, LightError> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(State::default())
    }
    async fn apply(&self, _change: &Change) -> Result<(), LightError> {
        tokio::time::sleep(Duration::from_secs(5)).await;
        Ok(())
    }
    async fn close(&self) -> Result<(), LightError> {
        Ok(())
    }
}

#[test]
fn test_a_slow_light_times_out_alone() {
    let calls: Calls = Arc::default();
    let mut make = fake_drivers(&calls, &[]);
    make.insert("triones".into(), Arc::new(|_lt: &Light| Arc::new(Slow) as Arc<dyn Driver>));
    make.remove("tuya");
    let h = Lights::with(vec![light("a", "govee"), light("b", "triones")], Some(make), Duration::from_millis(50));
    let out = rt().block_on(h.set("all", &change(Some(true), None, None)));
    assert_eq!(out["done"], json!(["a"]));
    assert_eq!(out["failed"], json!([{"name": "b", "reason": "it did not answer in time"}]));
    // a status read that times out says TimeoutError, as str(TimeoutError()) is empty in Python
    let st = rt().block_on(h.status("b"));
    assert_eq!(st["lights"][0]["reason"], json!("TimeoutError"));
}

#[test]
fn test_cli_name_sets_room_and_key() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("lights.json");
    save(&[Light { id: "x".into(), ip: "192.0.2.5".into(), ..light("tuya abcd", "tuya") }], Some(&p)).unwrap();
    let mut lines: Vec<String> = Vec::new();
    let args: Vec<String> =
        ["name", "tuya abcd", "desk bulb", "--room", "office", "--key", "secret"].iter().map(|s| s.to_string()).collect();
    assert_eq!(crate::cli(&args, Some(&p), &mut |l| lines.push(l.to_string())), 0);
    let lt = load(Some(&p)).remove(0);
    assert_eq!((lt.name.as_str(), lt.room.as_str(), lt.key.as_str()), ("desk bulb", "office", "secret"));
    assert!(!lines.join("\n").contains("secret"));
    assert_eq!(lines, ["desk bulb  [tuya]  room: office"]);
}

#[test]
fn cli_without_a_file_says_scan() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("lights.json");
    let mut lines: Vec<String> = Vec::new();
    assert_eq!(crate::cli(&["list".to_string()], Some(&p), &mut |l| lines.push(l.to_string())), 1);
    assert_eq!(lines, [format!("No lights in {}. Run: eve-bridge lights scan", p.display())]);
    assert_eq!(crate::cli(&["dance".to_string()], Some(&p), &mut |l| lines.push(l.to_string())), 2);
}

#[test]
fn cli_set_reports_an_unknown_colour() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("lights.json");
    save(&[light("a", "govee")], Some(&p)).unwrap();
    let mut lines: Vec<String> = Vec::new();
    let args: Vec<String> = ["set", "all", "--color", "plaid"].iter().map(|s| s.to_string()).collect();
    assert_eq!(crate::cli(&args, Some(&p), &mut |l| lines.push(l.to_string())), 1);
    assert_eq!(lines, ["{\n  \"ok\": false,\n  \"reason\": \"unknown colour 'plaid'\"\n}"]);
}

/// Review D5: status("all") read every Govee light at once, each on its own socket bound to the shared reply
/// port, so one light's reply could land in the other's socket and be thrown away. A fake light on localhost
/// answers each devStatus a little later, on the reply port: with the listen lock, each read gets its own.
#[test]
fn test_parallel_govee_reads_each_get_their_own_reply() {
    let light_sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let cmd = light_sock.local_addr().unwrap().port();
    let listen = UdpSocket::bind("0.0.0.0:0").unwrap().local_addr().unwrap().port();
    // the light answers the reads it has together: both at once when both asked within 200 ms (as two lights
    // answering the same moment), else the one alone
    light_sock.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        let mut pending = 0;
        let mut answered = 0;
        while answered < 2 {
            let quiet = match light_sock.recv_from(&mut buf) {
                Ok(_) => {
                    pending += 1;
                    false
                }
                Err(_) => true,
            };
            if pending == 2 || (quiet && pending > 0) {
                let reply = py::dumps(&json!({"msg": {"cmd": "devStatus", "data": {"onOff": 1, "brightness": 50}}}));
                for _ in 0..pending {
                    let _ = light_sock.send_to(reply.as_bytes(), ("127.0.0.1", listen));
                }
                answered += pending;
                pending = 0;
            }
        }
    });
    let ports = GoveePorts { scan: 0, listen, cmd };
    let drivers: Vec<Arc<GoveeDriver>> = ["a", "b"]
        .iter()
        .map(|n| Arc::new(GoveeDriver::with_ports(Light { ip: "127.0.0.1".into(), ..light(n, "govee") }, ports)))
        .collect();
    let threads: Vec<_> = drivers
        .iter()
        .map(|d| {
            let d = d.clone();
            std::thread::spawn(move || d.status_blocking())
        })
        .collect();
    for t in threads {
        let st = t.join().unwrap().unwrap();
        assert_eq!((st.on, st.brightness), (Some(true), Some(50)));
    }
}

#[test]
fn panel_drives_the_lights_and_says_what_happened() {
    // tests/test_miniapp.py, the lights card: the hub's part
    let rt = rt();
    let calls: Calls = Arc::default();
    let lights = vec![
        Light { room: "bedroom".into(), ip: "192.0.2.1".into(), device: "d".into(), ..light("floor lamp", "govee") },
        Light { room: "bedroom".into(), address: "u".into(), ..light("strip", "triones") },
    ];
    let h = Lights::with(lights, Some(fake_drivers(&calls, &["strip"])), Duration::from_secs(5));
    assert_eq!(h.listing(), vec![json!({"name": "floor lamp", "room": "bedroom"}), json!({"name": "strip", "room": "bedroom"})]);
    let status = rt.block_on(h.panel(&json!({"action": "status"})));
    assert_eq!(status["lights"].as_array().unwrap().len(), 2);
    let blue = rt.block_on(h.panel(&json!({"action": "set", "target": "all", "color": "blue"})));
    assert_eq!(blue, json!({"ok": true, "line": "Floor lamp blue. strip: unreachable.", "failed": ["strip"]}));
    let dim = rt.block_on(h.panel(&json!({"action": "set", "target": "floor lamp", "brightness": 40})));
    assert_eq!(dim["line"], json!("Floor lamp at 40%."));
    let bad = rt.block_on(h.panel(&json!({"action": "set", "color": "plaid"})));
    assert_eq!(bad, json!({"ok": false, "line": "Unknown colour 'plaid'."}));
    let nothing = rt.block_on(h.panel(&json!({"action": "set", "brightness": true})));
    assert_eq!(nothing, json!({"ok": false, "line": "Nothing to change."}));
    assert_eq!(
        *calls.lock(),
        vec![("floor lamp".to_string(), change(None, None, Some("blue"))), ("floor lamp".to_string(), change(None, Some(40), None))]
    );
}

#[test]
fn intent_eval_lights_off_is_a_command() {
    // tests/test_intent_eval.py: "lights off" is matched by code with one configured lamp
    let h = Lights::new(vec![Light { room: "office".into(), ..light("desk lamp", "govee") }]);
    let cmd = h.match_command("lights off").unwrap();
    assert_eq!(cmd.change, change(Some(false), None, None));
    assert_eq!(names(&cmd), ["desk lamp"]);
}

#[test]
fn line_names_everyone_only_when_all_answered() {
    let (h, _) = hub(&[]);
    let refs: Vec<&Light> = h.lights.iter().collect();
    let ok = |n: &str| Outcome { name: n.into(), ok: true, reason: String::new() };
    let results = vec![ok("floor lamp"), ok("strip"), ok("desk bulb")];
    assert_eq!(h.line(&refs, &results, &change(None, Some(30), None)), "Lights at 30%.");
    assert_eq!(h.line(&refs[..1], &results[..1], &change(Some(true), None, None)), "Floor lamp on.");
    assert_eq!(h.line(&[], &[], &change(Some(true), None, None)), "No lights changed.");
    assert_eq!(join_names(&["a", "b", "c"]), "A, b and c");
}

#[test]
fn tuya_helper_without_tinytuya_says_how_to_install_it() {
    let rt = rt();
    let req = json!({"op": "state", "light": {"id": "x", "ip": "192.0.2.5", "key": "k", "version": "3.3"}});
    let missing = rt.block_on(run_helper_with("/nonexistent/eve-python", &req)).unwrap_err();
    assert_eq!(missing.to_string(), NOT_INSTALLED);
    // an interpreter without the lights extra (a bare -S python has no site-packages)
    let bare = std::env::temp_dir().join(format!("eve-lights-bare-python-{}", std::process::id()));
    std::fs::write(&bare, "#!/bin/sh\nexec python3 -S -I \"$@\"\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bare, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let err = rt.block_on(run_helper_with(&bare.display().to_string(), &req)).unwrap_err();
    let _ = std::fs::remove_file(&bare);
    assert_eq!(err.to_string(), NOT_INSTALLED);
}

#[test]
#[ignore = "live: needs a Python with tinytuya in EVE_LIGHTS_PYTHON"]
fn tuya_helper_reaches_tinytuya() {
    let python = std::env::var("EVE_LIGHTS_PYTHON").unwrap_or_else(|_| "python3".into());
    let rt = rt();
    // no device answers at a documentation address: tinytuya's own error comes back as a Python exception
    let req = json!({"op": "apply", "light": {"id": "x", "ip": "192.0.2.5", "key": "0123456789abcdef", "version": "3.3"},
                     "calls": [["turn_on"]]});
    let err = rt.block_on(run_helper_with(&python, &req)).unwrap_err();
    assert_ne!(err.to_string(), NOT_INSTALLED, "{err:?}");
    let found = rt.block_on(async { crate::tuya::tuya_scan(1).await }).unwrap();
    assert!(found.iter().all(|d| !d.ip.is_empty()));
}

#[test]
#[ignore = "live: scans the LAN for Govee and WiZ lights"]
fn lan_scans_run() {
    let govee = crate::govee::govee_scan(2.0).unwrap();
    let wiz = crate::wiz::wiz_scan(2.0).unwrap();
    eprintln!("found {} Govee, {} WiZ", govee.len(), wiz.len());
    assert!(govee.iter().all(|d| !d.ip.is_empty() && !d.device.is_empty()));
    assert!(wiz.iter().all(|d| !d.ip.is_empty() && !d.device.is_empty()));
}

#[test]
#[ignore = "live: needs Bluetooth permission; macOS kills a process without it (exit 134)"]
fn bluetooth_scan_runs() {
    let found = rt().block_on(crate::triones::triones_scan(3.0)).unwrap();
    assert!(found.iter().all(|d| TRIONES_NAMES.iter().any(|p| d.name.starts_with(p))));
}

#[test]
#[ignore = "live: reads (never changes) the owner's Govee and WiZ lights from ~/.config/eve-bridge/lights.json"]
fn live_lan_lights_read() {
    let Some(h) = make_lights(None, Some(&HashMap::new())) else { return };
    let rt = rt();
    for kind in ["govee", "wiz"] {
        if h.lights.iter().any(|lt| lt.kind == kind) {
            let st = rt.block_on(h.status(kind));
            eprintln!("{}", py::dumps(&st));
            assert!(st["lights"].as_array().unwrap().iter().all(|l| l["reachable"] == json!(true)), "{st}");
        }
    }
    rt.block_on(h.close());
}

// ---- verification: Python 3.11+ timeouts, Python's string classes, argparse's int ----

struct TimesOut;

#[async_trait]
impl Driver for TimesOut {
    async fn state(&self) -> Result<State, LightError> {
        Err(LightError::Timeout("the WiZ light did not answer; is it on the same Wi-Fi?".into()))
    }

    async fn apply(&self, _change: &Change) -> Result<(), LightError> {
        Err(LightError::Timeout("the WiZ light did not answer; is it on the same Wi-Fi?".into()))
    }

    async fn close(&self) -> Result<(), LightError> {
        Ok(())
    }
}

#[test]
fn a_drivers_own_timeout_is_did_not_answer_in_time_for_a_change() {
    // asyncio.TimeoutError is TimeoutError since 3.11, so _one's handler takes a driver's TimeoutError too;
    // status keeps the driver's message (its handler is the generic one)
    let mut make: HashMap<String, DriverFactory> = HashMap::new();
    make.insert("wiz".into(), Arc::new(|_: &Light| Arc::new(TimesOut) as Arc<dyn Driver>));
    let h = Lights::with(vec![light("a", "wiz")], Some(make), Duration::from_secs(5));
    let rt = rt();
    let out = rt.block_on(h.set("all", &Change { power: Some(true), ..Change::default() }));
    assert_eq!(out["failed"], json!([{"name": "a", "reason": "it did not answer in time"}]));
    let st = rt.block_on(h.status("all"));
    assert_eq!(st["lights"][0]["reason"], json!("the WiZ light did not answer; is it on the same Wi-Fi?"));
}

#[test]
fn norm_and_colours_use_pythons_whitespace_words_and_digits() {
    assert_eq!(norm("lamp\u{bd} x\u{b2}\u{1c}y caf\u{e9}"), "lamp\u{bd} x\u{b2} y caf\u{e9}");
    assert_eq!(norm("cafe\u{301} lamp"), "cafe lamp");
    assert_eq!(norm("Lamp\u{203f}x"), "lamp x");
    let warm = parse_color(" warm\u{1c}white\u{1f}").unwrap();
    assert_eq!((warm.kelvin, warm.word.as_str()), (Some(2700), "warm white"));
    let full = parse_color("\u{ff12}\u{ff17}\u{ff10}\u{ff10}K").unwrap();
    assert_eq!((full.kelvin, full.word.as_str()), (Some(2700), "\u{ff12}\u{ff17}\u{ff10}\u{ff10}K"));
    let h = Lights::new(vec![light("floor lamp", "govee")]);
    let cmd = h.match_command("lamp \u{ff15}\u{ff10}%").unwrap();
    assert_eq!((cmd.change.brightness, names(&cmd)), (Some(50), vec!["floor lamp"]));
    let lt = Light::from_dict(&json!({"kind": " WIZ\u{1c}", "name": "\u{1f}a\u{1d}"})).unwrap();
    assert_eq!((lt.kind.as_str(), lt.name.as_str()), ("wiz", "a"));
    assert!(!enabled(Some(&HashMap::from([("EVE_LIGHTS".to_string(), "\u{1c}off".to_string())]))));
}

#[test]
fn a_partial_name_needs_pythons_word_boundary() {
    // "²" is a word character to Python, so "lamp" is not a whole word of "desk lamp²"
    let h = Lights::new(vec![light("desk lamp\u{b2}", "govee"), light("wiz", "wiz")]);
    let (found, why) = h.resolve("lamp");
    assert!(found.is_empty());
    assert_eq!(why, "no light called 'lamp'; the lights are: desk lamp\u{b2}, wiz");
    let h = Lights::new(vec![light("desk lamp", "govee"), light("wiz", "wiz")]);
    assert_eq!(h.resolve("lamp").0[0].name, "desk lamp");
    // re.search(rf"\b{re.escape(n)}\b", h) in Python 3.13, case by case
    for (hay, needle, want) in
        [("#1 desk", "#1", false), ("a#1", "#1", true), ("x#1 y", "#1", true), ("lamp\u{b2}", "lamp", false),
         ("lamp 2", "lamp", true)]
    {
        assert_eq!(py::contains_bounded(hay, needle), want, "{hay} {needle}");
    }
}

#[test]
fn cli_brightness_is_pythons_int_and_options_take_a_prefix() {
    let m = crate::cli::parser()
        .try_get_matches_from(["set", "all", "--bright", "\u{ff15}\u{ff10}"])
        .unwrap();
    let (_, sub) = m.subcommand().unwrap();
    assert_eq!(sub.get_one::<i64>("brightness").copied(), Some(50));
    let m = crate::cli::parser().try_get_matches_from(["set", "all", "--brightness", " +5_0 "]).unwrap();
    assert_eq!(m.subcommand().unwrap().1.get_one::<i64>("brightness").copied(), Some(50));
    assert!(crate::cli::parser().try_get_matches_from(["set", "all", "--brightness", "5.0"]).is_err());
}
