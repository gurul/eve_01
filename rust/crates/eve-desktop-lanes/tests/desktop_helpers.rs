//! desktop_helpers with fakes for every backend: app/URL opening, Vision box → point mapping, find/click/wait,
//! change detection, clipboard typing, zoom, the context and [after] lines, the 30 s wait cap and the install()
//! wiring (test_desktop_helpers.py); the lane adapter's AXPress fallback (test_ax_press_fallback.py); the adapter's
//! visual observation and pixel checker (test_decision_lane.py); and the Helpers half of the worker's settle
//! de-duplication and timing (test_desktop_worker.py).

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use image::{DynamicImage, Rgb, RgbImage};
use indexmap::IndexMap;

use eve_desktop_lanes::ax_candidates::{AxRef, Candidate, Rect, Snapshot};
use eve_desktop_lanes::decision_lane::DecisionSenses;
use eve_desktop_lanes::desktop_helpers::{
    self as dh, change_box, AxElementInfo, Backends, Clipboard, FocusedElement, Frame, Front, Gui, Helpers,
    LaneAdapter, LiveElement, OcrBox, TextLine, Waited, CHANGE_MIN_PIXELS, HELPER_NAMES, SETTLE_REUSE_SECS,
};
use eve_desktop_lanes::fast_lane::{Effectors, Senses};
use eve_desktop_lanes::error_kind;

// ---- fakes -------------------------------------------------------------------------

#[derive(Default)]
struct GuiLog {
    /// (x, y) for one click, (x, y, clicks) otherwise — as the Python fake records them
    clicks: Vec<Vec<i64>>,
    hotkeys: Vec<Vec<String>>,
    presses: Vec<String>,
}

struct FakeGui {
    points: (i64, i64),
    log: Arc<Mutex<GuiLog>>,
}

impl Gui for FakeGui {
    fn size(&self) -> (i64, i64) {
        self.points
    }

    fn click(&mut self, x: i64, y: i64, clicks: u32) -> anyhow::Result<()> {
        let rec = if clicks == 1 { vec![x, y] } else { vec![x, y, i64::from(clicks)] };
        self.log.lock().unwrap().clicks.push(rec);
        Ok(())
    }

    fn hotkey(&mut self, keys: &[&str]) -> anyhow::Result<()> {
        self.log.lock().unwrap().hotkeys.push(keys.iter().map(|k| k.to_string()).collect());
        Ok(())
    }

    fn press(&mut self, key: &str) -> anyhow::Result<()> {
        self.log.lock().unwrap().presses.push(key.to_string());
        Ok(())
    }
}

#[derive(Default)]
struct ClockState {
    t: f64,
    sleeps: Vec<f64>,
}

#[derive(Clone, Default)]
struct Clock(Arc<Mutex<ClockState>>);

impl Clock {
    fn now(&self) -> f64 {
        self.0.lock().unwrap().t
    }

    fn sleeps(&self) -> Vec<f64> {
        self.0.lock().unwrap().sleeps.clone()
    }

    fn sleep(&self, s: f64) {
        let mut st = self.0.lock().unwrap();
        st.sleeps.push(s);
        st.t = ((st.t + s) * 1e6).round() / 1e6;
    }

    fn clock_fn(&self) -> Box<dyn Fn() -> f64 + Send> {
        let c = self.clone();
        Box::new(move || c.now())
    }

    fn sleep_fn(&self) -> Box<dyn Fn(f64) + Send> {
        let c = self.clone();
        Box::new(move |s| c.sleep(s))
    }
}

#[derive(Default)]
struct ClipState {
    value: String,
    history: Vec<String>,
}

struct FakeClipboard(Arc<Mutex<ClipState>>);

impl Clipboard for FakeClipboard {
    fn paste(&mut self) -> anyhow::Result<String> {
        Ok(self.0.lock().unwrap().value.clone())
    }

    fn copy(&mut self, text: &str) -> anyhow::Result<()> {
        let mut st = self.0.lock().unwrap();
        st.value = text.to_string();
        st.history.push(text.to_string());
        Ok(())
    }
}

/// A fake that serves `values` in order; the last repeats (or the list cycles). Returns the counter of calls too.
fn scripted<T: Clone + Send + 'static>(values: Vec<T>, cycle: bool) -> (impl Fn() -> T + Send, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let f = move || {
        let i = counter.fetch_add(1, Ordering::SeqCst);
        if cycle {
            values[i % values.len()].clone()
        } else {
            values[i.min(values.len() - 1)].clone()
        }
    };
    (f, calls)
}

fn front(app: &str, title: &str, bundle: &str) -> Front {
    Front { app: app.to_string(), bundle: bundle.to_string(), title: title.to_string(), pid: 1 }
}

fn img_sized(w: u32, h: u32, color: [u8; 3]) -> RgbImage {
    RgbImage::from_pixel(w, h, Rgb(color))
}

fn img() -> RgbImage {
    img_sized(200, 100, [10, 20, 30])
}

/// A copy of `base` with a block covering `frac` of the width painted white (Python `with_block`).
fn with_block(base: &RgbImage, frac: f64) -> RgbImage {
    let mut out = base.clone();
    let (w, h) = base.dimensions();
    let bw = ((f64::from(w) * frac).round_ties_even() as u32).max(1);
    for y in 0..h {
        for x in 0..bw.min(w) {
            out.put_pixel(x, y, Rgb([255, 255, 255]));
        }
    }
    out
}

fn ocr_line(text: &str, bx: f64, by: f64, bw: f64, bh: f64) -> OcrBox {
    OcrBox { text: text.to_string(), bx, by, bw, bh, conf: 0.9 }
}

fn ocr_default(text: &str) -> OcrBox {
    ocr_line(text, 0.1, 0.8, 0.2, 0.1)
}

type OcrFake = Box<dyn Fn(&Frame, &str) -> anyhow::Result<Vec<OcrBox>> + Send>;

fn ocr_scripted(values: Vec<Vec<OcrBox>>) -> OcrFake {
    let (f, _) = scripted(values, false);
    Box::new(move |_frame: &Frame, _level: &str| Ok(f()))
}

struct Opts {
    points: (i64, i64),
    frames: Vec<RgbImage>,
    cycle: bool,
    ocr: Option<OcrFake>,
    fronts: Vec<Front>,
    rc: i32,
    menu_bar_points: i64,
}

impl Default for Opts {
    fn default() -> Self {
        Opts {
            points: (100, 50),
            frames: vec![img()],
            cycle: false,
            ocr: None,
            fronts: vec![front("Warp", "zsh", "")],
            rc: 0,
            menu_bar_points: 0,
        }
    }
}

/// One Helpers with every backend faked, plus the text and images it produced.
struct Bench {
    h: Helpers,
    gui: Arc<Mutex<GuiLog>>,
    clock: Clock,
    argv: Arc<Mutex<Vec<Vec<String>>>>,
    clip: Arc<Mutex<ClipState>>,
    logs: Arc<Mutex<Vec<String>>>,
    images: Arc<Mutex<Vec<RgbImage>>>,
    captures: Arc<AtomicUsize>,
}

impl Bench {
    fn new(opts: Opts) -> Bench {
        Bench::with(opts, |b| b)
    }

    /// `edit` adjusts the backends before the Helpers is built (Python assigned `h._element_at` etc. afterwards).
    fn with(opts: Opts, edit: impl FnOnce(Backends) -> Backends) -> Bench {
        let gui = Arc::new(Mutex::new(GuiLog::default()));
        let clock = Clock::default();
        let argv: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
        let clip = Arc::new(Mutex::new(ClipState { value: "old".to_string(), history: Vec::new() }));
        let frames: Vec<Arc<RgbImage>> = opts.frames.into_iter().map(Arc::new).collect();
        let (next_frame, captures) = scripted(frames, opts.cycle);
        let (next_front, _) = scripted(opts.fronts, false);
        let runner_argv = argv.clone();
        let rc = opts.rc;
        let backends = Backends {
            capture: Box::new(move || Ok(Frame::from_pil((*next_frame()).clone()))),
            ocr: opts.ocr.unwrap_or_else(|| ocr_scripted(vec![vec![]])),
            frontmost_fn: Box::new(next_front),
            run: Box::new(move |a: &[String], _timeout: f64| {
                runner_argv.lock().unwrap().push(a.to_vec());
                Ok(rc)
            }),
            clipboard: Some(Box::new(FakeClipboard(clip.clone()))),
            clock: clock.clock_fn(),
            sleep: clock.sleep_fn(),
            now: Box::new(|| {
                chrono::NaiveDate::from_ymd_opt(2026, 9, 6).unwrap().and_hms_opt(14, 2, 0).unwrap()
            }),
            // menu_bar_points=0: these frames are 100x50 points, so a 40-point menu bar would be most of them
            menu_bar_points: opts.menu_bar_points,
            element_at: Box::new(|_, _| Ok(None)),
            focused_element: Box::new(|| None),
            element_read: Box::new(|_| Ok(None)),
            element_press: Box::new(|_| Ok(0)),
            ..Backends::default()
        };
        let mut h = Helpers::new(Box::new(FakeGui { points: opts.points, log: gui.clone() }), edit(backends));
        h.ax_press_environ = Some(HashMap::new());
        let logs: Arc<Mutex<Vec<String>>> = Arc::default();
        let images: Arc<Mutex<Vec<RgbImage>>> = Arc::default();
        let (l, i) = (logs.clone(), images.clone());
        h.bind(Box::new(move |s| l.lock().unwrap().push(s.to_string())), Box::new(move |im| i.lock().unwrap().push(im)));
        Bench { h, gui, clock, argv, clip, logs, images, captures }
    }

    fn logs(&self) -> Vec<String> {
        self.logs.lock().unwrap().clone()
    }

    fn clicks(&self) -> Vec<Vec<i64>> {
        self.gui.lock().unwrap().clicks.clone()
    }

    fn argv(&self) -> Vec<Vec<String>> {
        self.argv.lock().unwrap().clone()
    }
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

fn kind_of(r: anyhow::Result<String>) -> String {
    error_kind(&r.expect_err("expected an error"))
}

// ---- open_app / open_url -----------------------------------------------------------

#[test]
fn test_open_app_runs_open_and_waits_for_frontmost() {
    let mut b = Bench::new(Opts {
        fronts: vec![front("Warp", "", ""), front("Warp", "", ""), front("Spotify", "", "")],
        ..Opts::default()
    });
    let result = b.h.open_app("Spotify", 8.0).unwrap();
    assert_eq!(b.argv(), vec![s(&["open", "-a", "Spotify"])]);
    assert!(result.contains("opened Spotify") && result.contains("after 0.2 s"), "{result}");
    let sleeps = b.clock.sleeps();
    assert_eq!(sleeps[..2], [0.1, 0.1]); // two polls, then the settle
    assert_eq!(sleeps[2], 0.25);
    assert_eq!(b.logs(), vec![result]);
}

#[test]
fn test_open_app_reports_when_not_frontmost_and_rejects_bad_names() {
    let mut b = Bench::new(Opts { fronts: vec![front("Warp", "", "")], ..Opts::default() });
    assert_eq!(b.h.open_app("Spotify", 1.0).unwrap(), "opened Spotify but Warp is still frontmost after 1.0 s");
    let mut bad = Bench::new(Opts { rc: 1, ..Opts::default() });
    let err = bad.h.open_app("Spotifyy", 8.0).unwrap_err();
    assert_eq!(error_kind(&err), "RuntimeError");
    assert!(err.to_string().contains("Spotifyy"));
    for name in ["../x", "-a"] {
        assert_eq!(kind_of(b.h.open_app(name, 8.0)), "ValueError");
    }
    assert_eq!(b.argv(), vec![s(&["open", "-a", "Spotify"])]); // the bad names never reached `open`
}

#[test]
fn test_open_url_argv_scheme_and_wait() {
    let mut b = Bench::new(Opts {
        fronts: vec![front("Warp", "zsh", ""), front("Warp", "zsh", ""), front("Safari", "Google", "com.apple.Safari")],
        ..Opts::default()
    });
    assert_eq!(b.h.open_url("https://x", None, 8.0).unwrap(), "opened https://x in Safari");
    assert_eq!(b.argv(), vec![s(&["open", "https://x"])]);
    let mut b2 = Bench::new(Opts {
        fronts: vec![front("Warp", "zsh", ""), front("Safari", "x", "com.apple.Safari")],
        ..Opts::default()
    });
    b2.h.open_url("https://x", Some("Safari"), 8.0).unwrap();
    assert_eq!(b2.argv(), vec![s(&["open", "-a", "Safari", "https://x"])]);
    // a non-browser whose title changed ends the wait too (e.g. a mailto: handler)
    let mut b3 = Bench::new(Opts {
        fronts: vec![front("Mail", "Inbox", ""), front("Mail", "Inbox", ""), front("Mail", "New Message", "")],
        ..Opts::default()
    });
    assert_eq!(b3.h.open_url("mailto:a@b.c", None, 8.0).unwrap(), "opened mailto:a@b.c in Mail");
    for url in ["file:///etc/passwd", "javascript:alert(1)"] {
        let err = b.h.open_url(url, None, 8.0).unwrap_err();
        assert_eq!(error_kind(&err), "ValueError");
        assert!(err.to_string().contains("only http, https and mailto"));
    }
    assert_eq!(b.argv(), vec![s(&["open", "https://x"])]);
}

// ---- screen_text / find_text / click_text ------------------------------------------

fn xywh(i: &TextLine) -> (i64, i64, i64, i64) {
    (i.x, i.y, i.w, i.h)
}

#[test]
fn test_screen_text_maps_vision_boxes_to_points_sorts_and_caps() {
    let mut b = Bench::new(Opts { ocr: Some(ocr_scripted(vec![vec![ocr_line("a", 0.1, 0.8, 0.2, 0.1)]])), ..Opts::default() });
    let out = b.h.screen_text(None, "accurate").unwrap();
    assert_eq!(out.len(), 1);
    assert_eq!(xywh(&out[0]), (10, 5, 20, 5));
    assert_eq!(b.logs(), vec!["screen_text: 1 lines"]);
    // the real Warp menu-bar box from the bench (bottom-left origin) lands at the top-left in points
    let mut real = Bench::new(Opts {
        points: (1512, 982),
        ocr: Some(ocr_scripted(vec![vec![ocr_line("Warp", 0.035, 0.975, 0.025, 0.013)]])),
        ..Opts::default()
    });
    let warp = real.h.screen_text(None, "accurate").unwrap().remove(0);
    assert!((warp.x - 53).abs() <= 1 && (warp.y - 11).abs() <= 1, "{warp:?}");
    // top-to-bottom, then left-to-right
    let lines = vec![
        ocr_line("bottom", 0.5, 0.1, 0.1, 0.05),
        ocr_line("top-right", 0.6, 0.9, 0.1, 0.05),
        ocr_line("top-left", 0.1, 0.9, 0.1, 0.05),
    ];
    let mut b = Bench::new(Opts { ocr: Some(ocr_scripted(vec![lines])), ..Opts::default() });
    let texts: Vec<String> = b.h.screen_text(None, "accurate").unwrap().into_iter().map(|i| i.text).collect();
    assert_eq!(texts, ["top-left", "top-right", "bottom"]);
    // region filter by centre
    let texts: Vec<String> =
        b.h.screen_text(Some((0, 40, 100, 10)), "accurate").unwrap().into_iter().map(|i| i.text).collect();
    assert_eq!(texts, ["bottom"]);
    // cap: 300 lines → 120 kept + a tail
    let many: Vec<OcrBox> =
        (0..300).map(|i| ocr_line(&format!("l{i}"), 0.0, 1.0 - (i as f64 + 1.0) * 0.003, 0.05, 0.002)).collect();
    let mut b = Bench::new(Opts { ocr: Some(ocr_scripted(vec![many])), ..Opts::default() });
    let out = b.h.screen_text(None, "accurate").unwrap();
    assert_eq!(out.len(), 121);
    assert!(out[120].text.contains("180 more lines"), "{}", out[120].text);
}

#[test]
fn test_find_text_whole_line_then_substring_casefold() {
    let lines = vec![ocr_line("Search results", 0.1, 0.5, 0.4, 0.1), ocr_line("Search", 0.1, 0.8, 0.2, 0.1)];
    let mut b = Bench::new(Opts { ocr: Some(ocr_scripted(vec![lines])), ..Opts::default() });
    let hit = b.h.find_text("search", None, "accurate").unwrap().unwrap();
    assert_eq!(hit.line.text, "Search");
    assert_eq!((hit.cx, hit.cy), (20, 7));
    assert_eq!(b.h.find_text("result", None, "accurate").unwrap().unwrap().line.text, "Search results");
    assert!(b.h.find_text("nope", None, "accurate").unwrap().is_none());
    let logs = b.logs();
    assert_eq!(logs[0], "found 'search' at (20,7)");
    assert_eq!(logs.last().unwrap(), "'nope' is not on screen");
}

#[test]
fn test_click_text_clicks_centre_and_raises_when_absent() {
    let mut b = Bench::new(Opts { ocr: Some(ocr_scripted(vec![vec![ocr_default("Search")]])), ..Opts::default() });
    assert_eq!(b.h.click_text("Search", None, 3.0, 1).unwrap(), "clicked 'Search' at (20,7)");
    assert_eq!(b.clicks(), vec![vec![20, 7]]);
    assert!(b.h.acted);
    let mut b = Bench::new(Opts::default());
    let err = b.h.click_text("Gone", None, 3.0, 1).unwrap_err();
    assert_eq!(error_kind(&err), "LookupError");
    assert!(err.to_string().contains("'Gone' is not on screen"));
    assert!((b.clock.now() - 3.0).abs() < 1e-9 && b.clicks().is_empty());
}

#[test]
fn test_click_text_finds_a_label_that_fast_ocr_misses_without_waiting() {
    let levels: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = levels.clone();
    let label = "Odetari x 9lives - | LOVE YOU HOE";
    let ocr: OcrFake = Box::new(move |_f: &Frame, level: &str| {
        seen.lock().unwrap().push(level.to_string());
        Ok(if level == "accurate" { vec![ocr_default(label)] } else { vec![] })
    });
    let mut b = Bench::new(Opts { ocr: Some(ocr), ..Opts::default() });
    assert!(b.h.find_text(label, None, "fast").unwrap().is_none()); // reproduce the old polling miss
    assert_eq!(b.h.screen_text(None, "accurate").unwrap()[0].text, label);
    levels.lock().unwrap().clear();
    b.h.click_text(label, None, 3.0, 1).unwrap();
    assert_eq!(b.clicks(), vec![vec![20, 7]]);
    assert_eq!(*levels.lock().unwrap(), ["accurate"]);
    assert!(b.clock.sleeps().is_empty());
}

#[test]
fn test_click_text_waits_for_accurate_label_and_respects_region_and_clicks() {
    let mut b = Bench::new(Opts {
        ocr: Some(ocr_scripted(vec![vec![], vec![ocr_line("Play", 0.1, 0.1, 0.2, 0.1)], vec![ocr_default("Play")]])),
        ..Opts::default()
    });
    b.h.click_text("Play", Some((0, 0, 50, 20)), 3.0, 2).unwrap();
    assert_eq!(b.clicks(), vec![vec![20, 7, 2]]);
    assert!((b.clock.now() - 0.6).abs() < 1e-9);
}

// ---- wait_for / wait_settled --------------------------------------------------------

#[test]
fn test_wait_for_polls_fast_then_confirms_accurate_and_supports_gone() {
    let levels: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = levels.clone();
    let (script, _) = scripted(
        vec![vec![], vec![], vec![ocr_line("Warp File", 0.0, 0.9, 0.2, 0.05)], vec![ocr_line("Warp", 0.0, 0.9, 0.1, 0.05)]],
        false,
    );
    let ocr: OcrFake = Box::new(move |_f: &Frame, level: &str| {
        seen.lock().unwrap().push(level.to_string());
        Ok(script())
    });
    let mut b = Bench::new(Opts { ocr: Some(ocr), ..Opts::default() });
    let Waited::Found(item) = b.h.wait_for("Warp", false, 6.0, 0.3, None).unwrap() else {
        panic!("expected the item");
    };
    assert_eq!(item.line.text, "Warp");
    assert_eq!(item.line.w, 10);
    assert_eq!(*levels.lock().unwrap(), ["fast", "fast", "fast", "accurate"]);
    assert_eq!(b.clock.sleeps(), [0.3, 0.3]);
    assert_eq!(b.logs(), ["'Warp' appeared after 0.6 s"]);
    let mut gone = Bench::new(Opts {
        ocr: Some(ocr_scripted(vec![vec![ocr_default("Saving")], vec![ocr_default("Saving")], vec![]])),
        ..Opts::default()
    });
    assert_eq!(gone.h.wait_for("Saving", true, 6.0, 0.3, None).unwrap(), Waited::Gone);
    let mut late = Bench::new(Opts::default());
    assert_eq!(late.h.wait_for("Inbox", false, 6.0, 0.3, None).unwrap(), Waited::NotSeen);
    assert_eq!(late.logs(), ["'Inbox' not seen in 6.0 s"]);
}

#[test]
fn test_wait_settled_and_change_box() {
    let a = img();
    let b_ = with_block(&a, 0.5);
    let c = with_block(&a, 0.55);
    let mut bench = Bench::new(Opts { frames: vec![a.clone(), b_.clone(), c.clone(), c], ..Opts::default() });
    assert!(bench.h.wait_settled(5.0, 0.25).unwrap());
    assert_eq!(bench.clock.sleeps(), [0.25, 0.25, 0.25]);
    assert_eq!(bench.logs(), ["screen settled after 0.8 s"]);
    let mut never = Bench::new(Opts { frames: vec![a.clone(), b_], cycle: true, ..Opts::default() });
    assert!(!never.h.wait_settled(2.0, 0.25).unwrap());
    assert_eq!(never.logs(), ["screen still changing after 2.0 s"]);
    // positive / negative control on the noise floor itself
    let thumb = Frame::from_pil(a.clone()).thumb().unwrap().clone();
    let mut one_px = a.clone();
    one_px.put_pixel(0, 0, Rgb([255, 255, 255]));
    assert_eq!(change_box(&thumb, Frame::from_pil(one_px).thumb().unwrap(), 0, CHANGE_MIN_PIXELS), None);
    assert!(change_box(&thumb, Frame::from_pil(with_block(&a, 0.05)).thumb().unwrap(), 0, CHANGE_MIN_PIXELS).is_some());
    let small = image::imageops::resize(&thumb, 10, 10, image::imageops::FilterType::CatmullRom);
    assert_eq!(change_box(&thumb, &small, 0, CHANGE_MIN_PIXELS), Some((0, 0, 10, 10))); // a size change is a change
}

/// A 1512x982-point screen at 2x, optionally with one white rectangle (physical pixels, both corners inclusive as
/// in ImageDraw.rectangle).
fn retina(draw: Option<(u32, u32, u32, u32)>) -> RgbImage {
    let mut im = img_sized(3024, 1964, [40, 40, 40]);
    if let Some((x0, y0, x1, y1)) = draw {
        for y in y0..=y1 {
            for x in x0..=x1 {
                im.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
    }
    im
}

#[test]
fn test_change_box_sees_a_checkbox_but_not_a_caret() {
    // The case the old 1/8-scale fraction missed (0.00022 < 0.003, measured 2026-09-10).
    let base = Frame::from_pil(retina(None)).thumb().unwrap().clone();
    let checkbox = Frame::from_pil(retina(Some((1000, 800, 1028, 828)))).thumb().unwrap().clone(); // 14x14 points
    let caret = Frame::from_pil(retina(Some((1000, 800, 1003, 832)))).thumb().unwrap().clone(); // 2x16 points
    assert!(change_box(&base, &checkbox, 0, CHANGE_MIN_PIXELS).is_some());
    assert_eq!(change_box(&base, &caret, 0, CHANGE_MIN_PIXELS), None);
    const { assert!(CHANGE_MIN_PIXELS > 8) }; // a caret is ~9 thumb pixels
}

#[test]
fn test_menu_bar_changes_do_not_count_and_regions_are_in_points() {
    let frames = vec![
        retina(None),
        retina(Some((2800, 10, 2900, 50))), // the clock ticks in the menu bar
        retina(Some((2800, 10, 2900, 50))),
        retina(Some((600, 1000, 700, 1100))),
    ];
    let mut b = Bench::new(Opts {
        points: (1512, 982),
        frames,
        fronts: vec![front("Safari", "Google", "")],
        menu_bar_points: dh::MENU_BAR_POINTS,
        ..Opts::default()
    });
    assert!(b.h.after_line().unwrap().ends_with("screen: unchanged")); // baseline
    assert!(b.h.after_line().unwrap().ends_with("screen: unchanged")); // menu bar only
    assert!(b.h.after_line().unwrap().ends_with("screen: unchanged"));
    let line = b.h.after_line().unwrap(); // a 50x50-point block at (300,500)
    assert!(line.contains("screen: changed around ("), "{line}");
    let inner = line.split("changed around (").nth(1).unwrap().split(')').next().unwrap();
    let v: Vec<i64> = inner.split(',').map(|p| p.trim().parse().unwrap()).collect();
    let (x, y, w, ht) = (v[0], v[1], v[2], v[3]);
    assert!((296..=300).contains(&x) && (496..=500).contains(&y), "{line}");
    assert!((50..=56).contains(&w) && (50..=56).contains(&ht), "{line}");
}

// ---- type_text / zoom / observe ------------------------------------------------------

#[test]
fn test_type_text_pastes_and_restores_clipboard() {
    let mut b = Bench::new(Opts::default());
    let line = b.h.type_text("café ☕", true).unwrap();
    assert_eq!(b.clip.lock().unwrap().history, ["café ☕", "old"]);
    assert_eq!(b.gui.lock().unwrap().hotkeys, vec![s(&["command", "v"])]);
    assert_eq!(b.gui.lock().unwrap().presses, ["enter"]);
    assert!(!line.contains("café"));
    assert_eq!(line, "typed 6 characters and pressed enter");
    assert!(b.h.acted);
    assert_eq!(b.clock.sleeps(), [0.15]);
}

#[test]
fn test_zoom_crops_physical_capture_and_logs_mapping() {
    let mut b = Bench::new(Opts::default());
    b.h.zoom(10, 10, 20, 5).unwrap();
    assert_eq!(b.images.lock().unwrap()[0].dimensions(), (40, 10));
    let log = &b.logs()[0];
    assert!(log.contains("points (10,10,20,5)") && log.contains("x+px/2"), "{log}");
    let mut big = Bench::new(Opts { points: (1000, 700), frames: vec![img_sized(2000, 1400, [10, 20, 30])], ..Opts::default() });
    big.h.zoom(0, 0, 900, 700).unwrap();
    assert_eq!(big.images.lock().unwrap()[0].dimensions(), (1600, 1200));
    assert!(big.logs()[0].contains("points (0,0,800,600)"));
}

#[test]
fn test_observe_context_and_after_line() {
    let a = img();
    let mut b = Bench::new(Opts {
        frames: vec![a.clone(), a.clone(), with_block(&a, 0.05)],
        fronts: vec![front("Safari", "Google", "")],
        ..Opts::default()
    });
    assert_eq!(b.h.context_line(), "frontmost: Safari — 'Google'; screen 100x50; 14:02");
    assert_eq!(b.h.observe("").unwrap(), b.h.context_line());
    assert_eq!(b.images.lock().unwrap().len(), 1);
    assert_eq!(b.images.lock().unwrap()[0].dimensions(), (100, 50));
    assert_eq!(b.logs(), vec![b.h.context_line()]);
    assert_eq!(b.h.after_line().unwrap(), "[after] frontmost: Safari — 'Google'; screen: unchanged");
    assert!(b
        .h
        .after_line()
        .unwrap()
        .starts_with("[after] frontmost: Safari — 'Google'; screen: changed around (0,0,"));
    b.h.acted = true; // the step clicked, typed or pressed keys
    assert_eq!(b.h.after_line().unwrap(), "[after your input] frontmost: Safari — 'Google'; screen: unchanged");
}

// ---- caps and wiring -------------------------------------------------------------------

#[test]
fn test_all_waits_are_capped_at_30s() {
    let mut b = Bench::new(Opts::default());
    b.h.wait_for("never", false, 999.0, 0.3, None).unwrap();
    assert!(b.clock.now() <= 30.0);
    let mut b = Bench::new(Opts { frames: vec![img(), with_block(&img(), 0.5)], cycle: true, ..Opts::default() });
    b.h.wait_settled(999.0, 0.25).unwrap();
    assert!(b.clock.now() <= 30.0);
    let mut b = Bench::new(Opts { fronts: vec![front("Warp", "", "")], ..Opts::default() });
    b.h.open_app("Spotify", 999.0).unwrap();
    let settles = b.clock.sleeps().iter().filter(|s| **s == 0.25).count();
    let polls_end = b.clock.now() - 0.25 * settles as f64; // minus the settle's intervals
    assert!(polls_end <= 30.0 && b.logs()[0].contains("after 30.0 s"), "{:?}", b.logs());
}

#[test]
fn test_install_binds_helpers_and_tracks_actions() {
    let mut b = Bench::new(Opts::default());
    let names = b.h.install();
    assert!(HELPER_NAMES.iter().all(|n| names.contains(n)));
    assert!(!b.h.acted);
    b.h.gui_hotkey(&["command", "l"]).unwrap();
    assert!(b.h.acted);
    assert_eq!(b.gui.lock().unwrap().hotkeys, vec![s(&["command", "l"])]);
    b.h.begin();
    assert!(!b.h.acted);
    b.h.screenshot().unwrap();
    assert!(!b.h.acted);
    b.h.install(); // idempotent: no double wrapping
    b.h.gui_click(1, 2, 1).unwrap();
    assert_eq!(b.clicks(), vec![vec![1, 2]]);
    assert_eq!(b.h.action_seq(), 2);
    for n in ["click", "hotkey", "press", "write"] {
        assert!(dh::ACTION_NAMES.contains(&n));
    }
}

// ---- click_element (handyman-style snap and verify) -----------------------------------------

fn el(role: &str, title: &str, frame: Option<Rect>, app: &str) -> AxElementInfo {
    AxElementInfo { role: role.to_string(), title: title.to_string(), frame, pressable: true, app: app.to_string() }
}

type Table = Arc<Mutex<HashMap<(i64, i64), AxElementInfo>>>;

fn table_bench(opts: Opts, table: &Table) -> Bench {
    let t = table.clone();
    Bench::with(opts, move |mut be| {
        be.element_at = Box::new(move |x, y| Ok(t.lock().unwrap().get(&(x, y)).cloned()));
        be
    })
}

fn table(entries: Vec<((i64, i64), AxElementInfo)>) -> Table {
    Arc::new(Mutex::new(entries.into_iter().collect()))
}

#[test]
fn test_click_element_snaps_to_the_control_centre_when_it_matches() {
    let t = table(vec![((30, 20), el("AXButton", "Sign in", Some((20, 10, 40, 20)), ""))]);
    let mut b = table_bench(Opts::default(), &t);
    let line = b.h.click_element(30, 20, "the Sign in button", 1).unwrap();
    assert_eq!(b.clicks(), vec![vec![40, 20]]);
    assert_eq!(line, "clicked AXButton 'Sign in' at (40,20)");
    assert!(b.h.acted);
}

#[test]
fn test_click_element_refuses_a_mismatch_and_says_what_is_there() {
    let t = table(vec![((30, 20), el("AXLink", "Weather", Some((20, 10, 40, 20)), ""))]);
    let mut b = table_bench(Opts::default(), &t);
    let line = b.h.click_element(30, 20, "Sign in button", 1).unwrap();
    assert!(b.clicks().is_empty());
    assert!(line.starts_with("did not click: under (30,20) is AXLink 'Weather', not 'Sign in button'"), "{line}");
    assert!(!b.h.acted);
}

#[test]
fn test_click_element_without_an_element_clicks_the_raw_point() {
    let t = table(vec![]);
    let mut b = table_bench(Opts::default(), &t);
    let line = b.h.click_element(7, 8, "anything", 1).unwrap();
    assert_eq!(b.clicks(), vec![vec![7, 8]]);
    assert!(line.contains("no accessibility element"));
}

#[test]
fn test_click_element_matches_untitled_fields_by_role_and_partial_words() {
    let t = table(vec![
        ((1, 1), el("AXTextField", "", None, "")),
        ((2, 2), el("AXButton", "Compose new message", None, "")),
    ]);
    let mut b = table_bench(Opts::default(), &t);
    assert!(b.h.click_element(1, 1, "the text field", 1).unwrap().starts_with("clicked AXTextField"));
    assert!(b
        .h
        .click_element(2, 2, "compose button", 1)
        .unwrap()
        .starts_with("clicked AXButton 'Compose new message'"));
}

#[test]
fn test_raw_clicks_report_their_target_in_the_after_line() {
    let a = img();
    let t = table(vec![((5, 6), el("AXLink", "Weather", None, ""))]);
    let mut b = table_bench(
        Opts { frames: vec![a.clone(), a.clone(), a], fronts: vec![front("Safari", "Google", "")], ..Opts::default() },
        &t,
    );
    b.h.install();
    b.h.begin();
    b.h.gui_click(5, 6, 1).unwrap();
    assert!(b.h.after_line().unwrap().ends_with("; clicked on AXLink 'Weather'"));
    b.h.begin();
    assert!(!b.h.after_line().unwrap().contains("clicked on"));
}

#[test]
fn test_raw_click_names_the_app_it_landed_in() {
    // Logged 2026-09-08: a click aimed at Spotify's Play landed on Warp's scroll area.
    let a = img();
    let mut scroll = el("AXScrollArea", "", None, "Warp");
    scroll.pressable = false;
    let t = table(vec![((128, 129), scroll), ((30, 20), el("AXLink", "Weather", None, "Safari"))]);
    let mut b = table_bench(
        Opts { frames: vec![a.clone(), a.clone(), a], fronts: vec![front("Warp", "zsh", "")], ..Opts::default() },
        &t,
    );
    b.h.install();
    b.h.begin();
    b.h.gui_click(128, 129, 1).unwrap();
    assert!(b.h.after_line().unwrap().ends_with("; clicked on AXScrollArea in Warp"));
    assert!(b
        .h
        .click_element(30, 20, "Play button", 1)
        .unwrap()
        .starts_with("did not click: under (30,20) is AXLink 'Weather' in Safari, not 'Play button'"));
}

#[test]
fn test_focused_pid_prefers_ax_then_window_list_then_workspace() {
    // NSWorkspace goes stale in the worker (no run loop): it must never win over a live source.
    let (ax, wl, ws) = (|| 11, || 22, || 33);
    let zero = || 0;
    assert_eq!(dh::focused_pid_from(&[&ax, &wl, &ws]), 11);
    assert_eq!(dh::focused_pid_from(&[&zero, &wl, &ws]), 22); // AX refused (-25204)
    assert_eq!(dh::focused_pid_from(&[&zero, &zero, &ws]), 33);
    assert_eq!(dh::focused_pid_from(&[&zero, &zero, &zero]), 0);
    assert_eq!(dh::ax_frontmost_for_pid(0), Front::default());
}

// ---- the fast lane's senses and effectors (delegate adapter) --------------------------------

fn candidate(label: &str, role: &str, frame: Rect, app: &str) -> Candidate {
    Candidate {
        id: "1".to_string(),
        role: role.to_string(),
        label: label.to_string(),
        value: String::new(),
        frame,
        actions: vec!["AXPress".to_string()],
        enabled: true,
        app: app.to_string(),
        in_content: false,
        in_dialog: false,
        secure: false,
        editable: role == "text field" || role == "search field",
    }
}

fn week(label: &str) -> Candidate {
    candidate(label, "radio button", (10, 10, 40, 20), "Calendar")
}

#[test]
fn test_install_binds_delegate_only_with_the_fast_lane() {
    let mut b = Bench::new(Opts::default());
    assert!(!b.h.install().contains(&"delegate"));
    b.h.fast_lane = true;
    assert!(b.h.install().contains(&"delegate"));
    b.h.fast_lane = false;
    assert!(!b.h.install().contains(&"delegate"));
}

#[test]
fn test_delegate_without_a_decider_is_unavailable_and_moves_nothing() {
    let mut b = Bench::new(Opts::default());
    b.h.lane_decide = "model".to_string(); // the path that needs the model; keyword mode does not
    let line = b.h.delegate(dh::DelegateCall { objective: "switch to week view".to_string(), ..Default::default() });
    assert!(line.starts_with("unavailable:"), "{line}");
    assert!(b.clicks().is_empty() && !b.h.acted);
    assert!(b.logs().last().unwrap().starts_with("delegate unavailable:"));
}

#[test]
fn test_click_candidate_re_hit_tests_and_refuses_on_every_mismatch() {
    let t = table(vec![((30, 20), el("AXRadioButton", "Week", Some((10, 10, 40, 20)), "Calendar"))]);
    let fail = Arc::new(Mutex::new(false));
    let (tt, ff) = (t.clone(), fail.clone());
    let mut b = Bench::with(Opts::default(), move |mut be| {
        be.element_at = Box::new(move |x, y| {
            if *ff.lock().unwrap() {
                return Err(eve_desktop_lanes::LaneError::err("RuntimeError", "ax down"));
            }
            Ok(tt.lock().unwrap().get(&(x, y)).cloned())
        });
        be
    });
    b.h.install();
    {
        let a = LaneAdapter::new(&mut b.h, None);
        assert_eq!(a.click_candidate(&week("Week")).unwrap(), "clicked AXRadioButton 'Week' in Calendar at (30,20)");
    }
    assert_eq!(b.clicks(), vec![vec![30, 20]]);
    assert!(b.h.acted && b.h.action_seq() == 1);
    {
        let a = LaneAdapter::new(&mut b.h, None);
        assert!(a.click_candidate(&week("Month")).unwrap().starts_with("refused: under (30,20) is AXRadioButton 'Week'"));
        let mail = candidate("Week", "radio button", (10, 10, 40, 20), "Mail");
        assert!(a.click_candidate(&mail).unwrap().starts_with("refused: (30,20) is in Calendar, not Mail"));
        let far = candidate("Week", "radio button", (100, 100, 10, 10), "Calendar");
        assert!(a.click_candidate(&far).unwrap().starts_with("refused: nothing under"));
    }
    t.lock().unwrap().insert((30, 20), el("AXButton", "Delete Event", Some((10, 10, 40, 20)), "Calendar"));
    let delete = candidate("Delete Event", "button", (10, 10, 40, 20), "Calendar");
    {
        let a = LaneAdapter::new(&mut b.h, None);
        assert_eq!(
            a.click_candidate(&delete).unwrap(),
            "refused: 'Delete Event' is a sensitive control without approval"
        );
    }
    {
        let approved = LaneAdapter::new(&mut b.h, Some("delete event"));
        assert!(approved.click_candidate(&delete).unwrap().starts_with("clicked AXButton 'Delete Event'"));
    }
    assert_eq!(b.clicks(), vec![vec![30, 20], vec![30, 20]]); // exactly the two allowed clicks
    *fail.lock().unwrap() = true;
    let a = LaneAdapter::new(&mut b.h, None);
    assert_eq!(a.click_candidate(&week("Week")).unwrap(), "refused: hit-test failed (RuntimeError)");
}

#[test]
fn test_focus_and_type_pastes_only_into_the_focused_matching_field() {
    let field = candidate("Search", "search field", (200, 10, 120, 22), "Calendar");
    let focus: Arc<Mutex<Option<FocusedElement>>> = Arc::new(Mutex::new(Some(FocusedElement {
        role: "AXSearchField".to_string(),
        title: "Search".to_string(),
        frame: Some((201, 10, 120, 22)),
        app: "Calendar".to_string(),
        editable: true,
    })));
    let f = focus.clone();
    let mut b = Bench::with(Opts::default(), move |mut be| {
        be.element_at = Box::new(|_, _| Ok(Some(el("AXSearchField", "Search", Some((200, 10, 120, 22)), "Calendar"))));
        be.focused_element = Box::new(move || f.lock().unwrap().clone());
        be
    });
    b.h.install();
    let a = LaneAdapter::new(&mut b.h, None);
    assert_eq!(a.focus_and_type(&field, "cats").unwrap(), "typed 4 characters");
    assert_eq!(b.gui.lock().unwrap().hotkeys, vec![s(&["command", "v"])]);
    assert_eq!(b.clip.lock().unwrap().history[0], "cats");
    *focus.lock().unwrap() = Some(FocusedElement {
        role: "AXTextArea".to_string(),
        title: "Message".to_string(),
        frame: Some((0, 300, 400, 100)),
        app: "Calendar".to_string(),
        editable: true,
    });
    let line = a.focus_and_type(&field, "cats").unwrap();
    assert!(line.starts_with("refused: focus moved to text area 'Message'"), "{line}");
    assert!(line.contains("at (0, 300, 400, 100)"), "{line}");
    *focus.lock().unwrap() = None;
    assert!(a.focus_and_type(&field, "cats").unwrap().starts_with("refused: focus is on nothing"));
    assert_eq!(b.gui.lock().unwrap().hotkeys, vec![s(&["command", "v"])]); // no second paste
}

#[test]
fn test_lane_senses_read_the_screen_and_the_snapshot() {
    let a = img();
    let thumb = |im: &RgbImage| Frame::from_pil(im.clone()).thumb().unwrap().clone();
    let mut b = Bench::new(Opts {
        frames: vec![a.clone(), a.clone(), a.clone(), with_block(&a, 0.3)], // two OCR captures first
        ocr: Some(ocr_scripted(vec![vec![ocr_default("Week view")]])),
        fronts: vec![front("Calendar", "September", "com.apple.iCal")],
        ..Opts::default()
    });
    let ad = LaneAdapter::new(&mut b.h, None);
    assert_eq!(Senses::frontmost_pid(&ad).unwrap(), 1);
    assert!(ad.text_visible("week").unwrap());
    assert!(!ad.text_visible("year").unwrap());
    assert!(!ad.text_visible("").unwrap());
    assert_eq!(ad.screen_changed().unwrap(), None); // no baseline before a snapshot
    ad.set_thumbs(None, Some(thumb(&a)));
    assert_eq!(ad.screen_changed().unwrap(), Some(false)); // one snapshot: against a fresh capture
    assert_eq!(ad.screen_changed().unwrap(), Some(true));
    // The lane asks AFTER its post-click snapshot. The step began at the snapshot before that, so the two
    // snapshots' frames are compared — not the settled screen with itself, which can never differ.
    ad.set_thumbs(Some(thumb(&a)), Some(thumb(&with_block(&a, 0.3))));
    assert_eq!(ad.screen_changed().unwrap(), Some(true));
    ad.set_thumbs(Some(thumb(&a)), Some(thumb(&a)));
    assert_eq!(ad.screen_changed().unwrap(), Some(false));
    assert_eq!(ad.press("return").unwrap(), "pressed return");
    assert!(ad.settle(0.5).unwrap() >= 0.0);
    drop(ad);
    assert_eq!(b.gui.lock().unwrap().presses, ["return"]);
    assert!(b.h.acted);
}

#[test]
fn test_click_candidate_accepts_a_hit_inside_a_row_labelled_by_its_static_text() {
    let row = candidate("Recently Added", "row", (20, 100, 200, 24), "Music");
    let t = table(vec![((120, 112), el("AXStaticText", "Recently Added", Some((60, 104, 120, 16)), "Music"))]);
    let mut b = table_bench(Opts::default(), &t);
    b.h.install();
    let set = |e: AxElementInfo| {
        t.lock().unwrap().insert((120, 112), e);
    };
    {
        let a = LaneAdapter::new(&mut b.h, None);
        assert!(a.click_candidate(&row).unwrap().starts_with("clicked AXStaticText 'Recently Added' in Music"));
        set(el("AXImage", "clock", Some((24, 104, 16, 16)), "Music")); // the row's icon
        assert!(a.click_candidate(&row).unwrap().starts_with("refused: under (120,112) is AXImage 'clock'")); // not in the label
        set(el("AXRow", "", Some((20, 100, 200, 24)), "Music")); // the row itself
        assert!(a.click_candidate(&row).unwrap().starts_with("clicked AXRow in Music"));
        set(el("AXStaticText", "Albums", Some((60, 130, 120, 16)), "Music")); // outside
        assert!(a.click_candidate(&row).unwrap().starts_with("refused: under (120,112) is AXStaticText 'Albums'"));
        set(el("AXStaticText", "Trash", Some((60, 104, 120, 16)), "Music"));
        let trash = candidate("Move to Trash", "row", (20, 100, 200, 24), "Music"); // matches, but sensitive
        assert!(a
            .click_candidate(&trash)
            .unwrap()
            .starts_with("refused: 'Trash' is a sensitive control without approval"));
    }
    assert_eq!(b.clicks(), vec![vec![120, 112], vec![120, 112]]);
}

// ---- the AXPress fallback (test_ax_press_fallback.py) -----------------------------------------

fn live(edit: impl FnOnce(&mut LiveElement)) -> LiveElement {
    let mut l = LiveElement {
        role: "AXRadioButton".to_string(),
        title: "Week".to_string(),
        frame: Some((10, 10, 40, 20)),
        enabled: true,
        pid: 42,
        actions: vec!["AXPress".to_string()],
    };
    edit(&mut l);
    l
}

struct Rig {
    b: Bench,
    c: Candidate,
    pressed: Arc<Mutex<Vec<AxRef>>>,
    reference: AxRef,
    snap: Snapshot,
}

enum RefKind {
    /// the snapshot holds the live element's ref
    Same,
    /// no ref for the candidate
    Missing,
    /// a ref that reads as gone
    Other,
}

fn rig(under: Option<AxElementInfo>, live_el: Option<LiveElement>, press_err: i64, refs: RefKind, cand: Option<Candidate>) -> Rig {
    let c = cand.unwrap_or_else(|| week("Week"));
    let reference = AxRef::new("the live AXUIElement");
    let pressed: Arc<Mutex<Vec<AxRef>>> = Arc::default();
    let p = pressed.clone();
    let read_ref = reference.clone();
    let live_el = live_el.unwrap_or_else(|| live(|_| {}));
    let mut b = Bench::with(Opts::default(), move |mut be| {
        be.element_at = Box::new(move |_, _| Ok(under.clone()));
        be.element_read = Box::new(move |el| Ok(if *el == read_ref { Some(live_el.clone()) } else { None }));
        be.element_press = Box::new(move |el| {
            p.lock().unwrap().push(el.clone());
            Ok(press_err)
        });
        be
    });
    b.h.install();
    let mut snap_refs = HashMap::new();
    match refs {
        RefKind::Same => {
            snap_refs.insert(c.id.clone(), reference.clone());
        }
        RefKind::Other => {
            snap_refs.insert(c.id.clone(), AxRef::new("another element"));
        }
        RefKind::Missing => {}
    }
    let snap = Snapshot {
        seq: 1,
        app: "Calendar".to_string(),
        bundle: "com.apple.iCal".to_string(),
        title: "w".to_string(),
        pid: 42,
        elements: vec![c.clone()],
        refs: snap_refs,
        ..Default::default()
    };
    Rig { b, c, pressed, reference, snap }
}

impl Rig {
    fn click(&mut self, c: &Candidate, approve: Option<&str>) -> String {
        let a = LaneAdapter::new(&mut self.b.h, approve);
        a.set_snapshot(Some(self.snap.clone()));
        a.click_candidate(c).unwrap()
    }

    fn pressed(&self) -> usize {
        self.pressed.lock().unwrap().len()
    }
}

#[test]
fn test_a_hit_test_miss_presses_the_snapshots_element_and_sends_no_click() {
    let mut r = rig(None, None, 0, RefKind::Same, None);
    let c = r.c.clone();
    let line = r.click(&c, None);
    assert!(line.starts_with("pressed AXRadioButton 'Week' with AXPress (the hit-test missed: nothing under"), "{line}");
    assert_eq!(*r.pressed.lock().unwrap(), vec![r.reference.clone()]);
    assert!(r.b.clicks().is_empty() && r.b.h.acted);
}

#[test]
fn test_an_overlay_on_the_point_is_a_miss_too() {
    let over = el("AXStaticText", "tooltip", Some((5, 5, 90, 40)), "Calendar");
    let mut r = rig(Some(over), None, 0, RefKind::Same, None);
    let c = r.c.clone();
    assert!(r.click(&c, None).starts_with("pressed AXRadioButton 'Week' with AXPress"));
    assert!(r.b.clicks().is_empty());
}

#[test]
fn test_a_hit_test_that_finds_the_control_clicks_and_never_also_presses() {
    let hit = el("AXRadioButton", "Week", Some((10, 10, 40, 20)), "Calendar");
    let mut r = rig(Some(hit), None, 0, RefKind::Same, None);
    let c = r.c.clone();
    assert!(r.click(&c, None).starts_with("clicked"));
    assert_eq!(r.b.clicks(), vec![vec![30, 20]]);
    assert_eq!(r.pressed(), 0);
}

#[test]
fn test_another_apps_window_over_the_point_is_still_refused() {
    let over = el("AXButton", "OK", Some((0, 0, 100, 100)), "Mail");
    let mut r = rig(Some(over), None, 0, RefKind::Same, None);
    let c = r.c.clone();
    assert!(r.click(&c, None).starts_with("refused: (30,20) is in Mail"));
    assert_eq!(r.pressed(), 0);
}

#[test]
fn test_an_element_that_is_no_longer_that_control_is_refused() {
    let cases = vec![
        live(|l| l.pid = 7),                                   // another process now
        live(|l| l.enabled = false),
        live(|l| l.actions = vec!["AXShowMenu".to_string()]), // nothing to press
        live(|l| l.frame = Some((10, 60, 40, 20))),           // moved more than 2 pt
        live(|l| l.frame = None),
        live(|l| l.title = "Month".to_string()), // renamed: a different control in the same place
    ];
    for l in cases {
        let mut r = rig(None, Some(l.clone()), 0, RefKind::Same, None);
        let c = r.c.clone();
        let line = r.click(&c, None);
        assert!(line.starts_with("refused: nothing under"), "{l:?}: {line}");
        assert_eq!(r.pressed(), 0);
        assert!(!r.b.h.acted);
    }
}

#[test]
fn test_a_stale_or_missing_element_is_refused() {
    let mut r = rig(None, None, 0, RefKind::Missing, None);
    let c = r.c.clone();
    assert!(r.click(&c, None).starts_with("refused:"));
    assert_eq!(r.pressed(), 0);
    let mut r = rig(None, None, 0, RefKind::Other, None); // reads as gone
    let c = r.c.clone();
    assert!(r.click(&c, None).starts_with("refused:"));
    assert_eq!(r.pressed(), 0);
}

#[test]
fn test_a_candidate_from_another_snapshot_is_refused() {
    let mut r = rig(None, None, 0, RefKind::Same, None);
    let other = candidate("Week", "radio button", (10, 10, 41, 20), "Calendar"); // same id, not this snapshot's
    assert!(r.click(&other, None).starts_with("refused:"));
    assert_eq!(r.pressed(), 0);
}

#[test]
fn test_a_sensitive_control_needs_approval() {
    let c = candidate("Delete Event", "button", (10, 10, 40, 20), "Calendar");
    let l = live(|l| {
        l.role = "AXButton".to_string();
        l.title = "Delete Event".to_string();
    });
    let mut r = rig(None, Some(l.clone()), 0, RefKind::Same, Some(c.clone()));
    assert_eq!(r.click(&c, None), "refused: 'Delete Event' is a sensitive control without approval");
    assert_eq!(r.pressed(), 0);
    let mut r = rig(None, Some(l), 0, RefKind::Same, Some(c.clone()));
    assert!(r.click(&c, Some("delete event")).starts_with("pressed AXButton 'Delete Event' with AXPress"));
    assert_eq!(*r.pressed.lock().unwrap(), vec![r.reference.clone()]);
}

#[test]
fn test_an_error_from_the_press_is_an_unknown_effect_not_a_refusal() {
    let mut r = rig(None, None, -25204, RefKind::Same, None);
    let c = r.c.clone();
    let line = r.click(&c, None);
    assert!(!line.starts_with("refused:") && line.contains("effect is unknown"), "{line}");
    assert!(r.b.h.acted);
}

#[test]
fn test_the_switch_turns_it_off() {
    let mut r = rig(None, None, 0, RefKind::Same, None);
    r.b.h.ax_press_environ = Some(HashMap::from([(dh::AX_PRESS_ENV.to_string(), "0".to_string())]));
    let c = r.c.clone();
    assert!(r.click(&c, None).starts_with("refused: nothing under"));
    assert_eq!(r.pressed(), 0);
}

#[test]
fn test_ax_press_on_reads_the_switch() {
    let env = |v: &str| HashMap::from([(dh::AX_PRESS_ENV.to_string(), v.to_string())]);
    for off in ["0", "false", " NO ", "off"] {
        assert!(!dh::ax_press_on(Some(&env(off))), "{off}");
    }
    for on in ["1", "true", "Yes", "on"] {
        assert!(dh::ax_press_on(Some(&env(on))), "{on}");
    }
    assert_eq!(dh::ax_press_on(Some(&env("maybe"))), dh::AX_PRESS_DEFAULT);
    assert_eq!(dh::ax_press_on(Some(&HashMap::new())), dh::AX_PRESS_DEFAULT);
}

// ---- the decision lane's senses (test_decision_lane.py) ----------------------------------------

#[test]
fn test_adapter_observation_uses_captured_retina_frame_in_screen_points() {
    let before = Snapshot {
        seq: 7,
        app: "Calendar".to_string(),
        bundle: "com.apple.iCal".to_string(),
        title: "September 2026".to_string(),
        pid: 42,
        elements: vec![week("Week")],
        ..Default::default()
    };
    let served = before.clone();
    let mut bench = Bench::with(
        Opts { points: (800, 500), frames: vec![img_sized(1600, 1000, [10, 20, 30])], ..Opts::default() },
        move |mut be| {
            be.ax_snapshot = Box::new(move |_| Ok(served.clone()));
            be
        },
    );
    let (s, image) = {
        let a = LaneAdapter::new(&mut bench.h, None);
        a.observe().unwrap()
    };
    assert_eq!(s, before);
    assert_eq!((image.width(), image.height()), (800, 500));
    assert_eq!(bench.captures.load(Ordering::SeqCst), 1);
}

#[test]
fn test_pixel_checker_has_positive_and_negative_controls() {
    let white = DynamicImage::ImageRgb8(img_sized(100, 100, [255, 255, 255]));
    let red = DynamicImage::ImageRgb8(img_sized(100, 100, [255, 0, 0]));
    assert!(LaneAdapter::image_matches(&white, &white.clone()));
    assert!(!LaneAdapter::image_matches(&white, &red));
    // only the menu bar differs: still the same picture
    let mut bar = img_sized(100, 100, [255, 255, 255]);
    bar.put_pixel(5, 5, Rgb([0, 0, 0]));
    assert!(LaneAdapter::image_matches(&white, &DynamicImage::ImageRgb8(bar)));
}

// ---- settle de-duplication and the timing dict (test_desktop_worker.py, the Helpers half) ----------

fn still() -> RgbImage {
    img_sized(200, 100, [10, 20, 30])
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

fn worker_bench(frames: Vec<RgbImage>) -> Bench {
    let mut b = Bench::new(Opts { frames, cycle: true, fronts: vec![front("Calendar", "x", "")], ..Opts::default() });
    b.h.install();
    b
}

#[test]
fn test_open_app_auto_screenshot_settles_once() {
    let mut b = worker_bench(vec![still()]);
    b.h.open_app("Calendar", 8.0).unwrap();
    b.h.settled_screenshot(1.5).unwrap(); // the worker's auto-screenshot
    assert!(b.clock.now() < 0.3, "{}", b.clock.now()); // one 0.25 s settle interval in total
    assert_eq!(b.clock.sleeps(), [0.25]);
    assert!(b.h.timing_ms()["settle"] >= 0.0);
}

#[test]
fn test_raw_click_still_settles_up_to_1_5s() {
    let mut b = worker_bench(moving());
    b.h.gui_click(1, 2, 1).unwrap();
    b.h.settled_screenshot(1.5).unwrap();
    let t = b.clock.now();
    assert!((1.5..1.8).contains(&t), "{t}"); // the screen never settled: full wait
}

#[test]
fn test_timed_out_settle_reuses_the_latest_frame_without_a_second_wait() {
    let mut b = worker_bench(moving());
    b.h.open_app("Calendar", 8.0).unwrap();
    let shot = b.h.settled_screenshot(1.5).unwrap();
    assert_eq!(b.clock.now(), 1.5);
    assert_eq!(shot.dimensions(), (100, 50));
    assert_eq!(shot.get_pixel(20, 20).0, [10, 20, 30]);
}

#[test]
fn test_explicit_settle_timeout_is_false_and_does_not_wait_again_for_video() {
    let mut b = worker_bench(moving());
    b.h.gui_click(1, 2, 1).unwrap();
    assert!(!b.h.wait_settled(5.0, 0.25).unwrap());
    assert!(b.logs().contains(&"screen still changing after 5.0 s".to_string()));
    b.h.settled_screenshot(1.5).unwrap();
    assert_eq!(b.clock.now(), 5.0);
}

#[test]
fn test_timed_out_settle_is_not_reused_after_a_later_input() {
    let mut b = worker_bench(moving());
    b.h.open_app("Calendar", 8.0).unwrap();
    b.h.gui_click(1, 2, 1).unwrap();
    b.h.settled_screenshot(1.5).unwrap();
    assert_eq!(b.clock.now(), 3.0);
}

#[test]
fn test_timed_out_settle_is_not_reused_when_the_frame_is_stale() {
    let mut b = worker_bench(moving());
    b.h.open_app("Calendar", 8.0).unwrap();
    b.clock.sleep(SETTLE_REUSE_SECS);
    b.h.settled_screenshot(1.5).unwrap();
    let t = b.clock.now();
    assert!((3.0 + SETTLE_REUSE_SECS..=3.25 + SETTLE_REUSE_SECS).contains(&t), "{t}");
}

#[test]
fn test_settle_is_not_reused_after_a_later_input() {
    let mut b = worker_bench(vec![still()]);
    b.h.open_app("Calendar", 8.0).unwrap();
    b.h.gui_click(1, 2, 1).unwrap();
    b.h.settled_screenshot(1.5).unwrap();
    assert_eq!(b.clock.sleeps(), [0.25, 0.25]); // the click after the settle needs its own
}

#[test]
fn test_reply_carries_timing_with_helpers() {
    let mut b = worker_bench(vec![still()]);
    b.h.begin();
    b.h.screen_text(None, "accurate").unwrap();
    let t = b.h.timing_ms();
    assert!(t.contains_key("capture") && t.contains_key("ocr"), "{t:?}");
    b.h.begin();
    b.h.gui_click(1, 2, 1).unwrap();
    b.h.settled_screenshot(1.5).unwrap();
    let t = b.h.timing_ms();
    assert!(t.contains_key("act") && t.contains_key("settle") && t.contains_key("resize"), "{t:?}");
}

#[test]
fn test_the_jev_asker_is_configured_only_by_the_owners_switches() {
    let mut b = Bench::new(Opts::default());
    assert!(b.h.step_asker.is_none());
    assert_eq!(b.h.lane_decide_mode(), "keyword");
    b.h.lane_decide = "jev".to_string();
    assert_eq!(b.h.lane_decide_mode(), "keyword"); // jev asked for, none configured: the gate decides alone
    let asker = |_: &str, _: &str, _: &str, _: &IndexMap<String, String>, _: &[String]| {
        eve_typed_ask::StepAnswer::new("none", 0.0, 0.0, 0.0, 0.0, 0.0)
    };
    b.h.step_asker = Some(Box::new(asker));
    assert_eq!(b.h.lane_decide_mode(), "jev");
    assert_eq!(b.h.script_decide(), "keyword"); // exact labels are the keyword gate's case
}

// ---- pure helpers -----------------------------------------------------------------------------

#[test]
fn test_what_matches_and_norm() {
    assert_eq!(dh::what(&el("AXButton", "Play", None, "Spotify")), "AXButton 'Play' in Spotify");
    assert_eq!(dh::what(&el("", "", None, "")), "element");
    assert_eq!(dh::what(&el("AXButton", "it's", None, "")), "AXButton \"it's\"");
    assert!(dh::matches("the blue Sign in button", "AXButton", "Sign in"));
    assert!(dh::matches("compose button", "AXButton", "Compose new message"));
    assert!(!dh::matches("Sign in button", "AXLink", "Weather"));
    assert!(dh::matches("the text field", "AXTextField", ""));
    assert_eq!(dh::norm("  Delete\n  Event "), "delete event");
}

#[test]
fn test_off_mac_backends_fail_like_the_missing_import() {
    if cfg!(target_os = "macos") {
        return;
    }
    assert_eq!(error_kind(&dh::cg_capture().unwrap_err()), "ModuleNotFoundError");
    assert_eq!(dh::ax_element_at(1, 1), None);
    assert_eq!(dh::ax_focused_element(), None);
    assert_eq!(dh::focused_pid(), 0);
}

#[test]
#[ignore = "live: needs Screen Recording and Accessibility on this Mac"]
fn live_capture_and_ocr_read_the_real_screen() {
    let frame = dh::cg_capture().unwrap();
    assert!(frame.width > 0 && frame.height > 0);
    assert_eq!(frame.pil().unwrap().dimensions(), (frame.width, frame.height));
    let boxes = dh::vision_ocr(&frame, "accurate").unwrap();
    assert!(!boxes.is_empty(), "a real screen always has text (the menu bar)");
    assert!(boxes.iter().all(|b| (0.0..=1.0).contains(&b.bx) && (0.0..=1.0).contains(&b.by)));
    assert!(dh::ax_frontmost().pid > 0);
}

#[test]
fn test_the_acting_wrap_covers_every_input_call() {
    fn assert_send<T: Send>() {}
    assert_send::<Helpers>();
    let t = table(vec![((3, 4), el("AXButton", "Play", None, "Music"))]);
    let mut b = table_bench(Opts::default(), &t);
    b.h.gui_action("scroll", Some((3, 4)), |_g| Ok(())).unwrap(); // before install(): the bare call
    assert!(!b.h.acted && b.h.action_seq() == 0);
    b.h.install();
    b.h.gui_action("scroll", Some((3, 4)), |_g| Ok(())).unwrap();
    assert!(b.h.acted && b.h.action_seq() == 1 && b.h.last_click().is_none()); // not a click: nothing described
    b.h.gui_action("doubleClick", Some((3, 4)), |g| g.click(3, 4, 2)).unwrap();
    assert_eq!(b.h.last_click(), Some("AXButton 'Play' in Music"));
    assert!(b.h.timing_ms().contains_key("act") && b.h.timing_ms().contains_key("ax"));
}

// ---- test_side_effect_precision.py: the planner's outline marks what the lane will ask for ----------

#[test]
fn test_the_outline_hint_uses_the_lanes_own_confirmation_rule() {
    let cand = |label: &str, role: &str, in_content: bool, cid: &str| Candidate {
        id: cid.into(),
        role: role.into(),
        label: label.into(),
        frame: (10, 20 * cid.parse::<i64>().unwrap(), 60, 16),
        actions: vec!["AXPress".into()],
        enabled: true,
        app: "Safari".into(),
        in_content,
        ..Candidate::default()
    };
    let rows = vec![
        cand("Follow", "button", true, "0"),
        cand("Star Wars", "link", true, "1"),
        cand("Delete", "button", false, "2"),
        cand("Save", "button", false, "3"),
    ];
    let s = Snapshot {
        seq: 1,
        app: "Safari".into(),
        bundle: "com.apple.Safari".into(),
        pid: 7,
        elements: rows,
        ..Snapshot::default()
    };
    let mut bench = Bench::with(Opts { points: (1440, 900), ..Opts::default() }, move |mut be| {
        be.ax_snapshot = Box::new(move |_| Ok(s.clone()));
        be
    });
    let out = bench.h.outline(80).unwrap();
    let lines: Vec<String> =
        out["lines"].as_array().unwrap().iter().map(|l| l.as_str().unwrap().to_string()).collect();
    let mut asked: Vec<String> = lines
        .iter()
        .filter(|l| l.ends_with("[asks the human first]"))
        .map(|l| l.split(" (").next().unwrap().to_string())
        .collect();
    asked.sort();
    assert_eq!(asked, vec!["Delete".to_string(), "Follow".to_string()], "{lines:?}");
}
