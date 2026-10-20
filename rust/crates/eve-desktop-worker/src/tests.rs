//! Port of bridge/tests/test_desktop_worker.py (and the worker case of test_plan_success_guard.py): exec semantics,
//! output shaping, Retina normalisation, error and fail-safe handling, the line protocol, the auto-screenshot rule
//! with helpers, the observe operation, --release, the settle de-duplication and timing seen from the worker, the
//! fast lane's wiring and the lane_first / outline / run_plan operations.
//!
//! The model's code runs in a real exec host on the bridge's Python (EVE_WORKER_PYTHON, else bridge/.venv) with the
//! Python test's FakeAutoGUI installed as `pyautogui`; the helpers are the real Rust Helpers on fake backends.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use base64::Engine as _;
use image::{Rgb, RgbImage};
use indexmap::IndexMap;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_decider::Choice;
use eve_desktop_lanes::ax_candidates::{Candidate, Snapshot};
use eve_desktop_lanes::desktop_helpers::{
    AxElementInfo, Backends, Clipboard, Frame, Front, Helpers, OcrBox, SETTLE_REUSE_SECS,
};
use eve_desktop_lanes::fast_lane::{LaneDecider, DEFAULT_STYLE, FAST_LANE_DEFAULT, LANE_FIRST_DEFAULT};
use eve_desktop_lanes::LaneError;

use super::*;

// ---- the fake PyAutoGUI (the Python test's own class) ---------------------------------------------------

const FAKE_AUTOGUI: &str = r#"
from PIL import Image


class FailSafeException(Exception):
    pass


class FakeAutoGUI:
    FailSafeException = FailSafeException

    def __init__(self, points=(100, 50), pixels=(200, 100)) -> None:
        self._points = points
        self._pixels = pixels
        self.clicks = []
        self.hotkeys = []
        self.presses = []

    def size(self):
        return self._points

    def screenshot(self, **kwargs):
        return Image.new("RGB", self._pixels, (10, 20, 30))

    def click(self, x, y, clicks=1):
        self.clicks.append((x, y))

    def hotkey(self, *keys):
        self.hotkeys.append(tuple(keys))

    def press(self, key):
        self.presses.append(key)
"#;

/// Python `_ns(gui)`: a namespace holding only `__builtins__` and a FakeAutoGUI.
fn ns_with(points: (i64, i64), pixels: (i64, i64)) -> Namespace {
    let ns = Namespace::spawn(&default_python()).expect("the exec host starts");
    ns.init(&[]).expect("init");
    let setup = format!(
        "_fake = {{}}\nexec({}, _fake)\npyautogui = _fake['FakeAutoGUI'](points=({}, {}), pixels=({}, {}))\ndel _fake",
        eve_decider::py_str_repr(FAKE_AUTOGUI),
        points.0,
        points.1,
        pixels.0,
        pixels.1
    );
    ns.setup(&setup).expect("the fake installs");
    ns
}

fn ns() -> Namespace {
    ns_with((100, 50), (200, 100))
}

fn texts(r: &Value) -> Vec<String> {
    r["output"].as_array().unwrap().iter().filter_map(|o| o["text"].as_str().map(str::to_string)).collect()
}

fn kinds(r: &Value) -> Vec<String> {
    r["output"].as_array().unwrap().iter().map(|o| o["type"].as_str().unwrap().to_string()).collect()
}

fn png_of(item: &Value) -> RgbImage {
    let url = item["image_url"].as_str().unwrap();
    let b64 = url.split_once(',').unwrap().1;
    let png = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
    assert!(png.starts_with(b"\x89PNG"));
    image::load_from_memory(&png).unwrap().to_rgb8()
}

fn run(ns: &Namespace, code: &str) -> Value {
    execute(code, ns, None).unwrap()
}

#[test]
fn test_log_and_stdout_become_text_items() {
    let r = run(&ns(), "log('a', 1)\nprint('b')\nprint()");
    assert_eq!(
        r,
        json!({"output": [{"type": "input_text", "text": "a 1"}, {"type": "input_text", "text": "b"}]})
    );
}

#[test]
fn test_display_image_becomes_png_data_url() {
    let r = run(&ns(), "display(pyautogui.screenshot())");
    let item = &r["output"][0];
    assert_eq!(item["type"], "input_image");
    assert_eq!(item["detail"], "original");
    assert_eq!(png_of(item).dimensions(), (200, 100)); // not normalised without normalize_screenshots
}

#[test]
fn test_normalize_screenshots_resizes_to_points_and_crops_regions() {
    let ns = ns_with((100, 50), (200, 100));
    normalize_screenshots(&ns).unwrap();
    assert_eq!(ns.eval("pyautogui.screenshot().size").unwrap(), json!([100, 50]));
    assert_eq!(ns.eval("pyautogui.screenshot(region=(10, 10, 20, 5)).size").unwrap(), json!([20, 5]));
}

#[test]
fn test_display_rejects_non_png() {
    let r = run(&ns(), "display(b'not a png')");
    assert!(r["output"][0]["text"].as_str().unwrap().contains("TypeError"), "{r}");
}

#[test]
fn test_globals_persist_between_calls() {
    let ns = ns();
    run(&ns, "x = 41");
    assert_eq!(run(&ns, "log(x + 1)")["output"], json!([{"type": "input_text", "text": "42"}]));
}

#[test]
fn test_exceptions_come_back_as_text_and_keep_partial_output() {
    let r = run(&ns(), "log('before')\n1/0");
    assert_eq!(r["output"][0]["text"], "before");
    assert!(r["output"][1]["text"].as_str().unwrap().contains("ZeroDivisionError"));
}

#[test]
fn test_failsafe_is_a_terminal_error() {
    let r = run(&ns(), "raise pyautogui.FailSafeException()");
    assert_eq!(r, json!({"error": {"code": "failsafe", "message": "desktop fail-safe: the mouse hit a screen corner"}}));
}

#[test]
fn test_no_output_placeholder() {
    assert_eq!(
        run(&ns(), "pass")["output"],
        json!([{"type": "input_text", "text": "exec_py completed with no output."}])
    );
}

#[test]
fn test_text_limit() {
    let r = run(&ns(), "log('x' * 70000)");
    let last = r["output"].as_array().unwrap().last().unwrap();
    assert!(last["text"].as_str().unwrap().contains("size limit"), "{r}");
}

#[test]
fn test_model_code_can_click() {
    let ns = ns();
    run(&ns, "pyautogui.click(3, 4)");
    assert_eq!(ns.eval("pyautogui.clicks").unwrap(), json!([[3, 4]]));
}

// ---- line protocol -----------------------------------------------------------------------------------

fn serve_lines(ns: &Namespace, lines: &[String], helpers: Option<&SharedHelpers>) -> Vec<Value> {
    let mut out: Vec<Value> = Vec::new();
    let input = lines.iter().map(|l| format!("{l}\n")).collect::<String>();
    serve(ns, input.as_bytes(), helpers, &mut |v: &Value| out.push(v.clone())).unwrap();
    out
}

#[test]
fn test_serve_executes_and_echoes_ids() {
    let out = serve_lines(&ns(), &[json!({"id": 7, "operation": "execute", "code": "log('hi')"}).to_string()], None);
    assert_eq!(out, vec![json!({"id": 7, "output": [{"type": "input_text", "text": "hi"}]})]);
}

#[test]
fn test_serve_rejects_bad_requests() {
    let out = serve_lines(
        &ns(),
        &[
            "not json".to_string(),
            json!({"id": 1, "operation": "ping"}).to_string(),
            json!({"id": 2, "operation": "execute", "code": "   "}).to_string(),
        ],
        None,
    );
    let codes: Vec<&str> = out.iter().map(|o| o["error"]["code"].as_str().unwrap()).collect();
    assert_eq!(codes, vec!["bad_json", "unsupported", "bad_code"]);
}

#[test]
fn test_serve_after_failsafe_refuses_everything() {
    let out = serve_lines(
        &ns(),
        &[
            json!({"id": 1, "operation": "execute", "code": "raise pyautogui.FailSafeException()"}).to_string(),
            json!({"id": 2, "operation": "execute", "code": "log('again')"}).to_string(),
        ],
        None,
    );
    assert_eq!(out[0]["error"]["code"], "failsafe");
    assert_eq!(out[1], json!({"id": 2, "error": out[0]["error"]}));
}

// ---- helpers: auto-screenshot, observe, --release -------------------------------------------------------

#[derive(Default)]
struct ClockState {
    t: f64,
    sleeps: Vec<f64>,
}

/// A fake clock that advances only with sleep (Python's `_Clock`), recording each sleep.
#[derive(Clone, Default)]
struct Clock(Arc<Mutex<ClockState>>);

impl Clock {
    fn now(&self) -> f64 {
        self.0.lock().t
    }

    fn sleep(&self, s: f64) {
        let mut st = self.0.lock();
        st.t = eve_desktop_lanes::py_round_n(st.t + s, 6);
        st.sleeps.push(s);
    }

    fn sleeps(&self) -> Vec<f64> {
        std::mem::take(&mut self.0.lock().sleeps)
    }
}

struct FakeClipboard(Arc<Mutex<String>>);

impl Clipboard for FakeClipboard {
    fn paste(&mut self) -> anyhow::Result<String> {
        Ok(self.0.lock().clone())
    }

    fn copy(&mut self, text: &str) -> anyhow::Result<()> {
        *self.0.lock() = text.to_string();
        Ok(())
    }
}

struct Bench {
    ns: Namespace,
    h: SharedHelpers,
    clock: Clock,
    captures: Arc<AtomicUsize>,
}

fn still() -> RgbImage {
    RgbImage::from_pixel(200, 100, Rgb([10, 20, 30]))
}

fn moving() -> Vec<RgbImage> {
    let a = still();
    let mut b = a.clone();
    for y in 0..100 {
        for x in 0..100 {
            b.put_pixel(x, y, Rgb([255, 255, 255]));
        }
    }
    vec![a, b]
}

fn front(app: &str, bundle: &str, title: &str, pid: i64) -> Front {
    Front { app: app.into(), bundle: bundle.into(), title: title.into(), pid }
}

/// Backends on fakes: a clock that advances with sleep, captures cycling over `frames`, `front_app` frontmost,
/// no AX element anywhere, and `open` that succeeds.
fn fake_backends(frames: Vec<RgbImage>, fronted: Front, ocr: Vec<OcrBox>, clock: &Clock, captures: &Arc<AtomicUsize>) -> Backends {
    let n = captures.clone();
    let (c1, c2) = (clock.clone(), clock.clone());
    Backends {
        capture: Box::new(move || {
            let i = n.fetch_add(1, Ordering::SeqCst);
            Ok(Frame::from_pil(frames[i % frames.len()].clone()))
        }),
        ocr: Box::new(move |_f, _level| Ok(ocr.clone())),
        frontmost_fn: Box::new(move || fronted.clone()),
        element_at: Box::new(|_, _| Ok(None)),
        focused_element: Box::new(|| None),
        element_read: Box::new(|_| Ok(None)),
        element_press: Box::new(|_| Err(LaneError::err("RuntimeError", "no AX in the tests"))),
        ax_snapshot: Box::new(|_| Err(LaneError::err("RuntimeError", "no AX in the tests"))),
        run: Box::new(|_, _| Ok(0)),
        clipboard: Some(Box::new(FakeClipboard(Arc::new(Mutex::new(String::new()))))),
        clock: Box::new(move || c1.now()),
        sleep: Box::new(move |s| c2.sleep(s)),
        now: Box::new(|| {
            chrono::NaiveDate::from_ymd_opt(2026, 9, 6).unwrap().and_hms_opt(14, 2, 0).unwrap()
        }),
        menu_bar_points: eve_desktop_lanes::desktop_helpers::MENU_BAR_POINTS,
        fast_lane: false,
        decider: None,
    }
}

fn bench_with(ns: Namespace, backends: Backends, clock: Clock, captures: Arc<AtomicUsize>) -> Bench {
    let mut h = Helpers::new(Box::new(HostGui::new(&ns)), backends);
    h.ax_press_environ = Some(HashMap::new());
    let h = Arc::new(Mutex::new(h));
    install(&mut h.lock(), &ns).unwrap();
    Bench { ns, h, clock, captures }
}

/// Python `_helpers(gui)`: a static screen, Warp frontmost, one OCR line.
fn helpers_bench() -> Bench {
    let (clock, captures) = (Clock::default(), Arc::new(AtomicUsize::new(0)));
    let ocr = vec![OcrBox { text: "Warp".into(), bx: 0.0, by: 0.9, bw: 0.1, bh: 0.05, conf: 0.9 }];
    let be = fake_backends(vec![still()], front("Warp", "dev.warp.Warp-Stable", "zsh", 1), ocr, &clock, &captures);
    bench_with(ns(), be, clock, captures)
}

/// Python `_bench(frames, front_app)`: frames cycle per capture, no OCR, the menu bar counts.
fn bench(frames: Vec<RgbImage>) -> Bench {
    let (clock, captures) = (Clock::default(), Arc::new(AtomicUsize::new(0)));
    let mut be = fake_backends(frames, front("Calendar", "", "x", 1), vec![], &clock, &captures);
    be.menu_bar_points = 0;
    bench_with(ns(), be, clock, captures)
}

impl Bench {
    fn exec(&self, code: &str) -> Value {
        execute(code, &self.ns, Some(&mut self.h.lock())).unwrap()
    }

    fn captured(&self) -> usize {
        self.captures.swap(0, Ordering::SeqCst)
    }
}

#[test]
fn test_auto_screenshot_after_actions_errors_and_empty_calls() {
    let b = helpers_bench();
    let r = b.exec("pyautogui.click(1,2)");
    // the call clicked, so the closing line is "[after your input]" (a call that only looked says "[after]")
    assert_eq!(kinds(&r), ["input_image", "input_text"]);
    assert!(r["output"][1]["text"].as_str().unwrap().starts_with("[after your input] frontmost: Warp"), "{r}");
    assert_eq!(b.clock.sleeps(), vec![0.25]); // one settle (1.5 s budget) on a still screen
    assert_eq!(b.ns.eval("pyautogui.clicks").unwrap(), json!([[1, 2]]));
    b.captured();
    let r = b.exec("x = screen_text()"); // text only: no image
    assert_eq!(kinds(&r), ["input_text", "input_text"]);
    assert_eq!(r["output"][0]["text"], "screen_text: 1 lines");
    assert!(b.clock.sleeps().is_empty());
    assert_eq!(b.captured(), 2); // OCR's capture and the [after] line's
    let r = b.exec("pass"); // empty: a picture, no wait
    assert_eq!(kinds(&r), ["input_image", "input_text"]);
    assert!(b.clock.sleeps().is_empty());
    assert_eq!(b.captured(), 2); // the picture and the [after] line
    let r = b.exec("display(pyautogui.screenshot())"); // displayed itself: exactly one image
    assert_eq!(kinds(&r), ["input_image", "input_text"]);
    assert_eq!(b.captured(), 1); // only the [after] line
    let r = b.exec("1/0");
    assert_eq!(kinds(&r), ["input_text", "input_image", "input_text"]);
    assert!(r["output"][0]["text"].as_str().unwrap().contains("ZeroDivisionError"));
    assert!(r["output"][2]["text"].as_str().unwrap().starts_with("[after]"));
    assert!(b.clock.sleeps().is_empty());
    assert_eq!(b.captured(), 2);
    let r = b.exec("type_text('hi')"); // helper actions count too
    assert_eq!(kinds(&r), ["input_text", "input_image", "input_text"]);
    assert_eq!(b.clock.sleeps(), vec![0.15, 0.25]); // the paste's pause, then the settle
    assert_eq!(b.ns.eval("pyautogui.hotkeys").unwrap(), json!([["command", "v"]]));
}

#[test]
fn test_serve_observe_operation() {
    let b = helpers_bench();
    let out = serve_lines(
        &b.ns,
        &[json!({"id": 1, "operation": "observe"}).to_string(), json!({"id": 2, "operation": "ping"}).to_string()],
        Some(&b.h),
    );
    assert_eq!(out[0]["id"], 1);
    assert_eq!(kinds(&out[0]).iter().filter(|k| *k == "input_image").count(), 1);
    assert_eq!(out[0]["output"][1]["text"], "frontmost: Warp — 'zsh'; screen 100x50; 14:02");
    assert_eq!(out[1]["error"]["code"], "unsupported");
    assert!(out[1]["error"]["message"].as_str().unwrap().contains("observe"));
}

#[derive(Default)]
struct FakeQuartz {
    posted: Vec<(u32, (String, i64, i64, i64))>,
}

impl Quartz for FakeQuartz {
    type Event = (String, i64, i64, i64);
    type Point = (i64, i64);

    fn create_keyboard_event(&mut self, code: u16, down: bool) -> Self::Event {
        ("key".into(), i64::from(code), i64::from(down), 0)
    }

    fn create(&mut self) -> Self::Event {
        ("ev".into(), 0, 0, 0)
    }

    fn get_location(&mut self, _event: &Self::Event) -> (i64, i64) {
        (3, 4)
    }

    fn create_mouse_event(&mut self, kind: u32, location: (i64, i64), button: u32) -> Self::Event {
        (format!("mouse {kind}"), location.0 * 10 + location.1, i64::from(button), 0)
    }

    fn post(&mut self, tap: u32, event: Self::Event) {
        self.posted.push((tap, event));
    }
}

#[test]
fn test_release_inputs_posts_key_and_mouse_ups() {
    let mut q = FakeQuartz::default();
    assert_eq!(release_inputs(&mut q), 131);
    let keys: Vec<_> = q.posted.iter().filter(|(_, e)| e.0 == "key").map(|(_, e)| e.clone()).collect();
    let mice: Vec<_> = q.posted.iter().filter(|(_, e)| e.0.starts_with("mouse")).map(|(_, e)| e.clone()).collect();
    assert_eq!(keys.len(), 128);
    assert!(keys.iter().all(|e| e.2 == 0)); // all key-ups
    assert_eq!(keys.iter().map(|e| e.1).collect::<Vec<_>>(), (0..128).collect::<Vec<i64>>());
    assert_eq!(
        mice,
        vec![
            ("mouse 2".to_string(), 34, 0, 0), // kCGEventLeftMouseUp at (3, 4), left button
            ("mouse 4".to_string(), 34, 1, 0), // right
            ("mouse 26".to_string(), 34, 2, 0), // other, centre button
        ]
    );
    assert!(q.posted.iter().all(|(tap, _)| *tap == K_CG_HID_EVENT_TAP));
}

// ---- settle de-duplication and the timing dict (fast-lane integration) ------------------------------------

#[test]
fn test_open_app_auto_screenshot_settles_once() {
    let b = bench(vec![still()]);
    let r = b.exec("open_app('Calendar')");
    assert_eq!(kinds(&r), ["input_text", "input_image", "input_text"]); // the reply still carries an image
    assert!(b.clock.now() < 0.3, "{}", b.clock.now()); // one 0.25 s settle interval in total
    assert!(r["timing"]["settle"].as_f64().unwrap() > 0.0 && r["timing"]["exec"].as_f64().unwrap() >= 0.0);
}

#[test]
fn test_raw_click_still_settles_up_to_1_5s() {
    let b = bench(moving());
    let r = b.exec("pyautogui.click(1, 2)");
    assert_eq!(kinds(&r), ["input_image", "input_text"]);
    let t = b.clock.now();
    assert!((1.5..1.8).contains(&t), "{t}"); // the screen never settled: full wait
}

#[test]
fn test_timed_out_settle_reuses_the_latest_frame_without_a_second_wait() {
    let b = bench(moving());
    let r = b.exec("open_app('Calendar')");
    assert_eq!(kinds(&r), ["input_text", "input_image", "input_text"]);
    assert_eq!(b.clock.now(), 1.5);
    assert_eq!(*png_of(&r["output"][1]).get_pixel(20, 20), Rgb([10, 20, 30]));
}

#[test]
fn test_explicit_settle_timeout_is_false_and_does_not_wait_again_for_video() {
    let b = bench(moving());
    let r = b.exec("pyautogui.click(1, 2); log(wait_settled())");
    let t = texts(&r);
    assert!(t.contains(&"False".to_string()) && t.contains(&"screen still changing after 5.0 s".to_string()), "{t:?}");
    assert!(kinds(&r).contains(&"input_image".to_string()));
    assert_eq!(b.clock.now(), 5.0);
}

#[test]
fn test_timed_out_settle_is_not_reused_after_a_later_input() {
    let b = bench(moving());
    b.exec("open_app('Calendar'); pyautogui.click(1, 2)");
    assert_eq!(b.clock.now(), 3.0);
}

#[test]
fn test_timed_out_settle_is_not_reused_when_the_frame_is_stale() {
    // Python's test lets its fake clock pass SETTLE_REUSE_SECS with a bare pause(); the clock is Rust's here, so a
    // wait_for that cannot succeed spends exactly that long (one poll, one interval) without any input.
    let b = bench(moving());
    b.exec(&format!("open_app('Calendar'); wait_for('nothing here', timeout={SETTLE_REUSE_SECS}, interval={SETTLE_REUSE_SECS})"));
    let t = b.clock.now();
    assert!((3.0 + SETTLE_REUSE_SECS..=3.25 + SETTLE_REUSE_SECS).contains(&t), "{t}");
}

#[test]
fn test_settle_is_not_reused_after_a_later_input() {
    let b = bench(vec![still()]);
    b.exec("open_app('Calendar'); pyautogui.click(1, 2)");
    // open_app's settle and the click's own: two 0.25 s intervals (a reused settle would be one)
    assert_eq!(b.clock.now(), 0.5);
}

#[test]
fn test_reply_carries_timing_with_helpers() {
    let b = bench(vec![still()]);
    let r = b.exec("x = screen_text()");
    let t = r["timing"].as_object().unwrap();
    for key in ["capture", "ocr", "encode", "exec"] {
        assert!(t.contains_key(key), "{t:?}");
    }
    assert!(t.values().all(Value::is_f64), "{t:?}");
    assert_eq!(t["encode"], 0.0); // a text-only call encodes nothing
    let r = b.exec("pyautogui.click(1, 2)");
    assert!(r["timing"]["encode"].as_f64().unwrap() > 0.0);
    assert!(r["timing"]["act"].as_f64().unwrap() >= 0.0);
    assert!(r["timing"].get("settle").is_some());
    assert!(run(&ns(), "log(1)").get("timing").is_none()); // no helpers: no timing
}

// ---- the fast lane's worker wiring ----------------------------------------------------------------------------

fn env(pairs: &[(&str, &str)]) -> Env {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

struct FakeDecider;

impl LaneDecider for FakeDecider {
    fn available(&self) -> bool {
        true
    }

    fn choose(
        &self,
        _objective: &str,
        _app: &str,
        _context: &str,
        _options: &IndexMap<String, String>,
        _recent: &[String],
    ) -> anyhow::Result<Choice> {
        Ok(Choice::default())
    }
}

#[test]
fn test_start_fast_lane_loads_in_a_thread_and_reports_status() {
    let b = bench(vec![still()]);
    let off = env(&[("EVE_FAST_LANE", "0"), ("EVE_LANE_FIRST", "0")]);
    assert_eq!(start_fast_lane(&b.h, &off, None, true), "off (EVE_FAST_LANE)");
    assert!(!b.h.lock().fast_lane);
    install(&mut b.h.lock(), &b.ns).unwrap();
    assert!(!b.ns.contains("delegate").unwrap());
    // model mode needs a decider the owner named; the keyword path (the default) needs none
    let model = env(&[("EVE_FAST_LANE", "1"), ("EVE_FAST_LANE_DECIDE", "model"), ("EVE_LANE_FIRST", "0")]);
    assert_eq!(start_fast_lane(&b.h, &model, None, true), "off (model mode needs EVE_DECIDER=jev)");
    assert!(!b.h.lock().fast_lane);
    let k = bench(vec![still()]);
    let keyword = env(&[("EVE_FAST_LANE", "1"), ("EVE_LANE_FIRST", "0")]);
    assert_eq!(start_fast_lane(&k.h, &keyword, None, true), "ready (keyword gate)");
    {
        let h = k.h.lock();
        assert!(h.fast_lane && h.decider.is_none());
        assert_eq!(h.lane_decide, "keyword");
        assert!(!h.lane_first_on);
    }
    // jev mode with no decider named: the step asker decides, so the lane is ready and nothing is loaded
    let j = bench(vec![still()]);
    let jev = env(&[("EVE_FAST_LANE", "1"), ("EVE_FAST_LANE_DECIDE", "jev"), ("EVE_LANE_FIRST", "0")]);
    assert_eq!(start_fast_lane(&j.h, &jev, None, true), "ready (jev step asker; no decider)");
    {
        let h = j.h.lock();
        assert!(h.fast_lane && h.decider.is_none());
        assert_eq!(h.lane_decide, "jev");
    }
    let calls = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = calls.clone();
    let loader: Loader = Box::new(move |style: &str| {
        seen.lock().push(style.to_string());
        Ok(Loaded { decider: Box::new(FakeDecider), load_ms: 400.0, warm_ms: 1500.0 })
    });
    let model_jev = env(&[
        ("EVE_FAST_LANE", "1"),
        ("EVE_DECIDER", "jev"),
        ("EVE_FAST_LANE_STYLE", "hinted"),
        ("EVE_FAST_LANE_DECIDE", "model"),
        ("EVE_LANE_FIRST", "0"),
    ]);
    let status = start_fast_lane(&b.h, &model_jev, Some(loader), false);
    assert!(status.starts_with("ready (load 400 ms, warm 1500 ms, style hinted)"), "{status}");
    assert!(b.h.lock().fast_lane && b.h.lock().decider.is_some());
    assert_eq!(*calls.lock(), vec!["hinted"]);
    install(&mut b.h.lock(), &b.ns).unwrap();
    assert!(b.ns.contains("delegate").unwrap());
    let r = b.exec("x = screen_text()");
    assert!(r["fast_lane"].as_str().unwrap().starts_with("ready ("), "{r}");
    assert!(r.get("timing").is_some());
    // a failing loader is a status line, never an exception, and the reply says so
    let broken: Loader = Box::new(|_style: &str| Err(LaneError::new("RuntimeError", "HTTP 503 from the decider")));
    let b2 = bench(vec![still()]);
    assert_eq!(
        start_fast_lane(&b2.h, &model_jev, Some(broken), false),
        "failed: RuntimeError: HTTP 503 from the decider"
    );
    install(&mut b2.h.lock(), &b2.ns).unwrap();
    assert_eq!(b2.exec("pass")["fast_lane"], "failed: RuntimeError: HTTP 503 from the decider");
}

#[test]
fn test_the_loader_thread_reports_when_it_is_done() {
    let b = bench(vec![still()]);
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let loader: Loader = Box::new(move |_style: &str| {
        let _ = rx.recv();
        Ok(Loaded { decider: Box::new(FakeDecider), load_ms: 1.0, warm_ms: 2.0 })
    });
    let model_jev =
        env(&[("EVE_FAST_LANE", "1"), ("EVE_DECIDER", "jev"), ("EVE_FAST_LANE_DECIDE", "model"), ("EVE_LANE_FIRST", "0")]);
    assert_eq!(start_fast_lane(&b.h, &model_jev, Some(loader), true), "loading");
    assert_eq!(b.exec("pass")["fast_lane"], "loading");
    tx.send(()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while b.h.lock().fast_lane_status == "loading" && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(b.exec("pass")["fast_lane"], "ready (load 1 ms, warm 2 ms, style jev)");
}

#[test]
fn test_fast_lane_config_defaults_follow_the_eval_decision() {
    assert_eq!(fast_lane_config(&env(&[])), (FAST_LANE_DEFAULT, DEFAULT_STYLE.to_string()));
    assert_eq!(DEFAULT_STYLE, "jev");
    assert_eq!(
        fast_lane_config(&env(&[("EVE_FAST_LANE", "yes"), ("EVE_FAST_LANE_STYLE", "turbo")])),
        (true, DEFAULT_STYLE.to_string())
    );
    assert_eq!(fast_lane_config(&env(&[("EVE_FAST_LANE_STYLE", "compact")])).1, "compact");
    assert!(!fast_lane_config(&env(&[("EVE_FAST_LANE", " OFF ")])).0);
}

#[test]
fn test_the_lane_is_off_away_from_macos() {
    let b = bench(vec![still()]);
    let on = env(&[("EVE_FAST_LANE", "1"), ("EVE_LANE_FIRST", "1")]);
    assert_eq!(start_fast_lane_on("linux", &b.h, &on, None, false), "off (not macOS: linux)");
    assert!(!b.h.lock().lane_first_on && !b.h.lock().fast_lane);
}

// ---- the lane_first operation ----------------------------------------------------------------------------------

fn cand(cid: &str, role: &str, label: &str, value: &str) -> Candidate {
    let x = 100 + 70 * cid.parse::<i64>().unwrap();
    let editable = ["text field", "search field", "text area", "combo box"].contains(&role);
    Candidate {
        id: cid.into(),
        role: role.into(),
        label: label.into(),
        value: value.into(),
        frame: (x, 100, 60, 30),
        actions: if editable { vec![] } else { vec!["AXPress".into()] },
        enabled: true,
        app: "Calendar".into(),
        in_content: false,
        in_dialog: false,
        secure: false,
        editable,
    }
}

static SEQ: AtomicUsize = AtomicUsize::new(0);

/// test_fast_lane.calendar(): Calendar's toolbar, Month (or Week) selected.
fn calendar(week_selected: bool) -> Snapshot {
    let cands = vec![
        cand("0", "button", "Add Event", ""),
        cand("1", "radio button", "Day", ""),
        cand("2", "radio button", "Week", if week_selected { "selected" } else { "" }),
        cand("3", "radio button", "Month", if week_selected { "" } else { "selected" }),
        cand("4", "button", "Today", ""),
        cand("5", "button", "Delete Event", ""),
        cand("6", "search field", "Search", ""),
    ];
    let count = cands.len() as i64 + 3;
    Snapshot {
        seq: SEQ.fetch_add(1, Ordering::SeqCst) as i64 + 1,
        app: "Calendar".into(),
        bundle: "com.apple.iCal".into(),
        title: "September 2026".into(),
        pid: 42,
        elements: cands,
        context_lines: vec!["title: September 2026".into()],
        node_count: count,
        secs: 0.05,
        ..Snapshot::default()
    }
}

/// Python `_lane_bench`: scripted AX snapshots (the last repeats), a hit-test that finds the Week radio button under
/// its own centre, and Calendar (pid 42) in front, so the real Helpers → adapter → router path runs.
fn lane_bench(snapshots: Vec<Snapshot>) -> Bench {
    let (clock, captures) = (Clock::default(), Arc::new(AtomicUsize::new(0)));
    let mut be = fake_backends(vec![still()], front("Calendar", "com.apple.iCal", "x", 42), vec![], &clock, &captures);
    be.menu_bar_points = 0;
    let served = Arc::new(Mutex::new(VecDeque::from(snapshots)));
    let last = Arc::new(Mutex::new(None::<Snapshot>));
    be.ax_snapshot = Box::new(move |_screen| {
        let mut q = served.lock();
        let mut l = last.lock();
        if let Some(s) = q.pop_front() {
            *l = Some(s);
        }
        Ok(l.clone().expect("a snapshot"))
    });
    be.element_at = Box::new(|_, _| {
        Ok(Some(AxElementInfo {
            role: "AXRadioButton".into(),
            title: "Week".into(),
            frame: Some((240, 100, 60, 30)),
            pressable: true,
            app: "Calendar".into(),
        }))
    });
    bench_with(ns(), be, clock, captures)
}

#[test]
fn test_lane_first_operation_replies_with_the_route() {
    let b = lane_bench(vec![calendar(false), calendar(true)]);
    let status = start_fast_lane(&b.h, &env(&[("EVE_LANE_FIRST", "1")]), None, true);
    if SYS_PLATFORM != "darwin" {
        return; // the router runs on macOS only
    }
    assert_eq!(status, "off (EVE_FAST_LANE); router on");
    assert!(b.h.lock().lane_first_on && !b.h.lock().fast_lane); // the router does not need the helper
    let out = serve_lines(
        &b.ns,
        &[
            json!({"id": 1, "operation": "lane_first", "goal": "switch to week view"}).to_string(),
            json!({"id": 2, "operation": "lane_first"}).to_string(),
            json!({"id": 3, "operation": "execute", "code": "log('still serving')"}).to_string(),
        ],
        Some(&b.h),
    );
    let route = &out[0]["lane_first"];
    assert_eq!(out[0]["id"], 1);
    assert_eq!(route["status"], "complete", "{route}");
    assert_eq!(route["clicked"], json!(["Week"]));
    assert_eq!(route["sentence"], "Done. I clicked Week.");
    assert!(route["line"].as_str().unwrap().starts_with("script: complete"));
    let log: Vec<&str> = route["log"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
    assert!(log.iter().any(|l| l.starts_with("delegate step 1: Week (radio button)")), "{log:?}");
    assert_eq!(b.ns.eval("pyautogui.clicks").unwrap(), json!([[270, 115]]));
    assert!(out[0]["timing"].get("ax").is_some(), "{}", out[0]["timing"]);
    assert_eq!(
        out[1],
        json!({"id": 2, "lane_first": {"status": "unavailable", "reason": "lane_first needs a string goal"}})
    );
    assert_eq!(out[2]["output"][0], json!({"type": "input_text", "text": "still serving"})); // never terminal
}

#[test]
fn test_lane_first_without_the_lane_says_unavailable() {
    let b = lane_bench(vec![calendar(false)]);
    assert_eq!(start_fast_lane(&b.h, &env(&[("EVE_LANE_FIRST", "0")]), None, true), "off (EVE_FAST_LANE)");
    assert!(!b.h.lock().lane_first_on);
    let reply = lane_first(&json!({"goal": "switch to week view"}), Some(&mut b.h.lock()));
    assert_eq!(reply, json!({"lane_first": {"status": "unavailable", "reason": "the router is off (EVE_LANE_FIRST)"}}));
    assert_eq!(b.ns.eval("pyautogui.clicks").unwrap(), json!([]));
    assert_eq!(lane_first(&json!({"goal": "x"}), None)["lane_first"]["status"], "unavailable");
    // a router that fails is "the planner does it", with zero input
    let (clock, captures) = (Clock::default(), Arc::new(AtomicUsize::new(0)));
    let mut be = fake_backends(vec![still()], front("Calendar", "com.apple.iCal", "x", 42), vec![], &clock, &captures);
    be.ax_snapshot = Box::new(|_| panic!("ax down"));
    let broken = bench_with(ns(), be, clock, captures);
    broken.h.lock().lane_first_on = true;
    let reply = lane_first(&json!({"goal": "switch to week view"}), Some(&mut broken.h.lock()));
    assert_eq!(reply["lane_first"]["status"], "none");
    assert_eq!(reply["lane_first"]["reason"], "error:panic");
    assert_eq!(broken.ns.eval("pyautogui.clicks").unwrap(), json!([]));
    // the default is the eval's decision
    assert_eq!(lane_modes(&env(&[])), ("keyword".to_string(), LANE_FIRST_DEFAULT));
    assert_eq!(
        lane_modes(&env(&[("EVE_FAST_LANE_DECIDE", "model"), ("EVE_LANE_FIRST", "on")])),
        ("model".to_string(), true)
    );
    assert_eq!(
        lane_modes(&env(&[("EVE_FAST_LANE_DECIDE", "nonsense"), ("EVE_LANE_FIRST", "off")])),
        ("keyword".to_string(), false)
    );
}

#[test]
fn test_unknown_operation_is_still_unsupported() {
    let out = serve_lines(&ns(), &[json!({"id": 1, "operation": "lane_second", "goal": "x"}).to_string()], None);
    assert_eq!(out[0]["error"]["code"], "unsupported");
    assert!(out[0]["error"]["message"].as_str().unwrap().contains("lane_first"));
}

#[test]
fn test_decider_backend_selects_jev_only_on_the_model_path() {
    assert_eq!(decider_backend(&env(&[])), "");
    assert_eq!(decider_backend(&env(&[("EVE_DECIDER", "JEV")])), "jev");
    assert_eq!(decider_backend(&env(&[("EVE_DECIDER", "gpt")])), "");
    let base = [("EVE_FAST_LANE", "1"), ("EVE_DECIDER", "jev"), ("EVE_LANE_FIRST", "0"), ("EVE_JEV_ROUTE", "openrouter")];
    // keyword mode: the gate decides, so a hosted model is not loaded at all — nothing leaves the Mac
    let opened = Arc::new(Mutex::new(Vec::<String>::new()));
    let seen = opened.clone();
    let opener: Arc<dyn eve_jev::Opener> = Arc::new(move |req: &eve_jev::Request, _t: f64| {
        seen.lock().push(format!("{} {}", req.url, String::from_utf8_lossy(&req.data)));
        Err(eve_jev::OpenError::Http(503))
    });
    let mut keyed = env(&base);
    keyed.insert("OPENROUTER_API_KEY".into(), "k".into());
    let h = bench(vec![still()]);
    let status = start_fast_lane(&h.h, &keyed, Some(jev_loader(keyed.clone(), Some(opener.clone()))), false);
    assert_eq!(status, "ready (keyword gate)");
    assert!(opened.lock().is_empty() && h.h.lock().decider.is_none());
    // model mode: Jev loads through eve_jev::load on the env's route, in the style it scored best with
    let mut model = keyed.clone();
    model.insert("EVE_FAST_LANE_DECIDE".into(), "model".into());
    let h2 = bench(vec![still()]);
    let status = start_fast_lane(&h2.h, &model, Some(jev_loader(model.clone(), Some(opener))), false);
    assert!(status.starts_with("failed: RuntimeError: jev warm-up predict failed"), "{status}");
    let calls = opened.lock().clone();
    assert!(!calls.is_empty() && calls.iter().all(|c| c.contains("openrouter.ai")), "{calls:?}");
    // the default loader (no seam) reads the same env: here the route's key is missing, so nothing is sent
    let mut nokey = env(&base);
    nokey.insert("EVE_FAST_LANE_DECIDE".into(), "model".into());
    let h3 = bench(vec![still()]);
    assert_eq!(
        start_fast_lane(&h3.h, &nokey, None, false),
        "failed: RuntimeError: OPENROUTER_API_KEY is not set (route openrouter)"
    );
}

// ---- the outline and run_plan operations ----------------------------------------------------------------------

#[test]
fn test_outline_and_run_plan_run_through_the_real_helpers() {
    let b = lane_bench(vec![calendar(false), calendar(false), calendar(true)]);
    // No switch set: no hosted model is configured, and the executor falls back to the keyword gate alone.
    assert_eq!(start_jev_asker(&mut b.h.lock(), &env(&[])), "off");
    assert!(b.h.lock().step_asker.is_none());
    let plan = json!({"steps": [{"kind": "click", "target": "the week view", "label_hint": "Week"}],
                      "final_say": "Week it is."});
    let out = serve_lines(
        &b.ns,
        &[
            json!({"id": 1, "operation": "outline"}).to_string(),
            json!({"id": 2, "operation": "run_plan", "plan": plan, "request": "week view"}).to_string(),
            json!({"id": 3, "operation": "run_plan"}).to_string(),
            json!({"id": 4, "operation": "run_plan", "plan": {"steps": [{"kind": "drag"}]}}).to_string(),
            json!({"id": 5, "operation": "execute", "code": "log('still serving')"}).to_string(),
        ],
        Some(&b.h),
    );
    let shown = &out[0]["outline"];
    let lines: Vec<&str> = shown["lines"].as_array().unwrap().iter().map(|l| l.as_str().unwrap()).collect();
    assert_eq!(shown["app"], "Calendar");
    assert!(lines.contains(&"Week (radio button)"), "{lines:?}");
    assert!(lines.iter().any(|l| l.starts_with("Delete Event") && l.contains("asks the human first")), "{lines:?}");
    assert!(!lines.iter().any(|l| l.contains("September 2026"))); // never the window title
    let ran = &out[1]["run_plan"];
    assert_eq!(ran["status"], "complete", "{ran}");
    assert_eq!(ran["sentence"], "Week it is.");
    assert_eq!(b.ns.eval("pyautogui.clicks").unwrap(), json!([[270, 115]]));
    assert_eq!(ran["ledger"][0]["effect"], "confirmed");
    assert_eq!(ran["ledger"][0]["how"], "keyword");
    assert!(out[1]["timing"].get("ax").is_some());
    assert_eq!(out[2]["run_plan"]["status"], "unavailable");
    assert_eq!(out[3]["run_plan"]["status"], "unavailable");
    assert!(out[3]["run_plan"]["reason"].as_str().unwrap().contains("bad plan"), "{}", out[3]);
    assert_eq!(out[4]["output"][0], json!({"type": "input_text", "text": "still serving"})); // never terminal
}

#[test]
fn test_the_jev_asker_is_configured_only_by_the_owners_switches() {
    let b = bench(vec![still()]);
    let mut h = b.h.lock();
    assert_eq!(start_jev_asker(&mut h, &env(&[("OPENROUTER_API_KEY", "k"), ("EVE_JEV_ROUTE", "openrouter")])), "off");
    assert!(start_jev_asker(&mut h, &env(&[("EVE_PLAN_EXEC", "1")])).starts_with("off (TYPESAFE_API_KEY is not set"));
    assert!(h.step_asker.is_none());
    assert_eq!(h.lane_decide_mode(), "keyword");
    h.lane_decide = "jev".into();
    assert_eq!(h.lane_decide_mode(), "keyword"); // jev asked for, none configured: the gate decides alone
    let status = start_jev_asker(
        &mut h,
        &env(&[("EVE_PLAN_EXEC", "1"), ("OPENROUTER_API_KEY", "k"), ("EVE_JEV_ROUTE", "openrouter")]),
    );
    assert_eq!(status, "ready (typesafe/jev-1.13)");
    assert!(h.step_asker.is_some());
    assert_eq!(h.lane_decide_mode(), "jev");
    assert_eq!(h.script_decide(), "keyword"); // exact labels are the keyword gate's case
}

// ---- test_plan_success_guard.py ----------------------------------------------------------------------------------

#[test]
fn test_the_desktop_worker_passes_the_rejection_to_the_helpers() {
    let req = json!({"plan": {"steps": []}, "request": "x", "start": 1, "success_rejected": true});
    let args = run_plan_args(&req).unwrap();
    assert!(args.success_rejected);
    assert_eq!((args.request.as_str(), args.start, args.dry_run), ("x", 1, false));
    assert!(!run_plan_args(&json!({"plan": {}})).unwrap().success_rejected);
}

// ---- parity details ----------------------------------------------------------------------------------------------

#[test]
fn test_a_request_over_512_kib_is_refused_and_serving_goes_on() {
    let big = json!({"id": 1, "operation": "execute", "code": format!("log('{}')", "x".repeat(MAX_REQUEST_BYTES))});
    let out = serve_lines(
        &ns(),
        &[big.to_string(), json!({"id": 2, "operation": "execute", "code": "log(2)"}).to_string()],
        None,
    );
    assert_eq!(out[0], json!({"error": {"code": "request_too_large", "message": "request exceeds 512 KiB"}}));
    assert_eq!(out[1]["output"][0]["text"], "2");
}

#[test]
fn test_code_over_64_kib_is_bad_code() {
    let code = format!("# {}", "x".repeat(MAX_CODE_BYTES));
    let out = serve_lines(&ns(), &[json!({"id": 9, "operation": "execute", "code": code}).to_string()], None);
    assert_eq!(out[0]["error"]["code"], "bad_code");
    assert_eq!(out[0]["id"], 9);
}

#[test]
fn test_a_bad_start_ends_the_worker_as_pythons_except_did() {
    let b = bench(vec![still()]);
    let req = json!({"plan": {"steps": []}, "start": "abc"});
    let e = run_plan(&req, Some(&mut b.h.lock())).unwrap_err();
    assert_eq!((e.kind.as_str(), e.message.as_str()), ("ValueError", "invalid literal for int() with base 10: 'abc'"));
    let mut sink = Vec::new();
    let line = format!("{}\n", json!({"id": 1, "operation": "run_plan", "plan": {}, "start": "abc"}));
    assert!(serve(&b.ns, line.as_bytes(), Some(&b.h), &mut |v: &Value| sink.push(v.clone())).is_err());
    assert!(sink.is_empty());
}

#[test]
fn test_int_reads_like_python() {
    let int = |v: Value| py_int_or_zero(Some(&v)).map_err(|e| e.kind);
    assert_eq!(int(json!(null)), Ok(0));
    assert_eq!(int(json!("")), Ok(0));
    assert_eq!(int(json!(false)), Ok(0));
    assert_eq!(int(json!(true)), Ok(1));
    assert_eq!(int(json!(3.9)), Ok(3));
    assert_eq!(int(json!(-2.5)), Ok(-2));
    assert_eq!(int(json!(" 12 ")), Ok(12));
    assert_eq!(int(json!("1_000")), Ok(1000));
    assert_eq!(int(json!("-7")), Ok(-7));
    assert_eq!(int(json!("007")), Ok(7));
    assert_eq!(int(json!("_7")), Err("ValueError".into()));
    assert_eq!(int(json!("000")), Ok(0));
    assert_eq!(int(json!("3.0")), Err("ValueError".into()));
    assert_eq!(int(json!([1])), Err("TypeError".into()));
    assert_eq!(py_int_or_zero(None), Ok(0));
}

#[test]
fn test_helper_arguments_are_bound_as_python_binds_them() {
    let b = helpers_bench();
    // a region must unpack into four numbers
    let r = b.exec("screen_text(region=(1, 2, 3))");
    assert!(r["output"][0]["text"].as_str().unwrap().contains("ValueError: not enough values to unpack (expected 4, got 3)"), "{r}");
    // the signature is Python's: an unknown keyword is the stub's TypeError
    let r = b.exec("open_app('Calendar', speed=2)");
    assert!(r["output"][0]["text"].as_str().unwrap().contains("TypeError"), "{r}");
    // values come back as Python values
    let r = b.exec("f = frontmost(); log(type(f).__name__, f['app'], f['pid'])");
    assert_eq!(texts(&r)[1], "dict Warp 1");
    let r = b.exec("hit = find_text('warp'); log(hit['cx'], hit['cy'], hit['text'])");
    assert_eq!(texts(&r)[1], "5 3 Warp");
    let r = b.exec("log(wait_for('nope', timeout=0.3, interval=0.3) is None)");
    assert!(texts(&r).contains(&"True".to_string()), "{r}");
    // a helper's own error is its Python class
    let r = b.exec("click_text('absent', timeout=0)");
    assert!(r["output"][0]["text"].as_str().unwrap().contains("LookupError: 'absent' is not on screen"), "{r}");
}

#[test]
fn test_a_helpers_log_past_the_limit_raises_in_the_models_code() {
    let b = helpers_bench();
    let r = b.exec("log('x' * 61430)\ntry:\n    frontmost()\n    caught = 'no'\nexcept ValueError as e:\n    caught = str(e)");
    // frontmost's own line could not be added, so the helper's call raised in the model's code
    assert_eq!(r["output"][0]["text"].as_str().unwrap().len(), 61430);
    assert_eq!(b.exec("log(caught)")["output"][0]["text"], "text output exceeds its size limit");
}

#[test]
fn test_the_namespace_survives_a_failsafe_inside_a_helper() {
    // a fail-safe raised by PyAutoGUI under a helper is the same terminal error as a raw one
    let b = helpers_bench();
    b.ns.setup("def _boom(*a, **k):\n    raise pyautogui.FailSafeException()\npyautogui.hotkey = _boom").unwrap();
    // re-wrap: install binds the replaced hotkey
    install(&mut b.h.lock(), &b.ns).unwrap();
    let r = b.exec("type_text('hi')");
    assert_eq!(r, json!({"error": {"code": "failsafe", "message": "desktop fail-safe: the mouse hit a screen corner"}}));
}

#[test]
fn test_lone_surrogates_read_as_replacement_characters() {
    assert_eq!(host::replace_lone_surrogates(r#"{"t": "\ud800x"}"#), r#"{"t": "\ufffdx"}"#);
    assert_eq!(host::replace_lone_surrogates(r#"{"t": "\ud83d\ude00"}"#), r#"{"t": "\ud83d\ude00"}"#);
    assert_eq!(host::replace_lone_surrogates(r#"{"t": "\\ud800"}"#), r#"{"t": "\\ud800"}"#);
    let r = run(&ns(), "log('a\\ud800b')");
    assert_eq!(r["output"][0]["text"], "a\u{fffd}b");
}

#[test]
fn test_a_dead_host_is_host_died() {
    let ns = ns();
    let e = execute("import os\nos._exit(3)", &ns, None).unwrap_err();
    assert!(e.downcast_ref::<HostDied>().is_some(), "{e}");
}

#[test]
fn test_the_models_stdin_and_stdout_cannot_reach_the_pipe() {
    let ns = ns();
    let r = run(&ns, "import sys, os\nos.write(1, b'stray\\n')\nlog(repr(sys.stdin.read()))");
    assert_eq!(texts(&r), vec!["''"]);
    assert_eq!(run(&ns, "log('fine')")["output"][0]["text"], "fine");
}

#[test]
fn test_threads_of_the_models_code_take_turns_on_the_pipe() {
    // Python's log() only appended to a list, so the model's own threads could log freely; here they talk over
    // the pipe, one at a time, and every line lands in the call's output
    let ns = ns();
    let code = "import threading\n\
                def worker(i):\n    for j in range(25):\n        log('t', i, j)\n\
                ts = [threading.Thread(target=worker, args=(i,)) for i in range(4)]\n\
                [t.start() for t in ts]\n[t.join() for t in ts]\nlog('joined')";
    let r = run(&ns, code);
    let t = texts(&r);
    assert_eq!(t.iter().filter(|l| l.starts_with("t ")).count(), 100, "{t:?}");
    assert_eq!(t.last().unwrap(), "joined");
    assert_eq!(run(&ns, "log('fine')")["output"], json!([{"type": "input_text", "text": "fine"}]));
}

#[test]
fn test_a_thread_that_outlives_its_call_cannot_corrupt_the_next_one() {
    let b = helpers_bench();
    let code = "import threading, time\n\
                def late():\n    time.sleep(0.2)\n    log('late')\n    print('late print')\n    pyautogui.click(5, 6)\n    \
                try:\n        frontmost()\n    except RuntimeError as e:\n        globals()['late_err'] = str(e)\n\
                threading.Thread(target=late).start()";
    b.exec(code);
    std::thread::sleep(std::time::Duration::from_millis(800));
    let r = b.exec("log(late_err)");
    let t = texts(&r);
    assert_eq!(
        t[0],
        "frontmost() ran after its exec_py call ended; the desktop helpers answer only while a call is running"
    );
    assert!(!t.iter().any(|l| l.contains("late")), "{t:?}"); // the late log went nowhere, as in Python
    assert_eq!(b.ns.eval("pyautogui.clicks").unwrap(), json!([[5, 6]])); // the input itself still ran
    assert_eq!(b.exec("log('next')")["output"][0], json!({"type": "input_text", "text": "next"}));
}

#[test]
fn test_a_script_delegate_ignores_the_objectives_arguments() {
    let b = lane_bench(vec![calendar(false), calendar(true)]);
    let on = env(&[("EVE_FAST_LANE", "1"), ("EVE_LANE_FIRST", "0")]);
    if start_fast_lane(&b.h, &on, None, false) != "ready (keyword gate)" {
        return; // the lane runs on macOS only
    }
    install(&mut b.h.lock(), &b.ns).unwrap();
    let r = b.exec("log(delegate(steps=['Week'], text=5, key=1, done_when=2, dry_run=True))");
    let t = texts(&r);
    assert!(!t.iter().any(|l| l.contains("TypeError")), "{t:?}");
    assert_eq!(b.ns.eval("pyautogui.clicks").unwrap(), json!([])); // a dry run clicks nothing
}
