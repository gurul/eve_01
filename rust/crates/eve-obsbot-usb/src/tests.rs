//! Port of bridge/tests/test_obsbot_usb.py: building the helper, reading its JSON, asleep by the parked tilt,
//! waking, the turn's directions (the camera's relative "+1" pans the other way, measured 2026-10-11), and finding
//! an OBSBOT on the USB bus.

use super::*;
use parking_lot::Mutex;
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

#[derive(Default)]
struct HelperState {
    argv: Vec<Vec<String>>,
    tilt: f64,
    error: String,
}

/// subprocess.run for the helper: records argv, answers with the JSON a Tiny 3 gave.
#[derive(Clone, Default)]
struct Helper(Arc<Mutex<HelperState>>);

impl Helper {
    fn new(tilt: f64, error: &str) -> Self {
        Helper(Arc::new(Mutex::new(HelperState { argv: vec![], tilt, error: error.into() })))
    }

    fn argv(&self) -> Vec<Vec<String>> {
        self.0.lock().argv.clone()
    }
}

impl Runner for Helper {
    fn run(&self, argv: &[String], _timeout: Duration) -> io::Result<RunOutput> {
        let mut st = self.0.lock();
        st.argv.push(argv[1..].to_vec());
        let out = if !st.error.is_empty() {
            json!({"ok": false, "error": st.error})
        } else if argv[1] == "get" {
            json!({"ok": true, "name": "OBSBOT Tiny 3", "pan": 0.0, "tilt": st.tilt, "zoom": 0, "zoom_max": 100})
        } else {
            json!({"ok": true})
        };
        Ok(RunOutput { returncode: 0, stdout: format!("{out}\n"), stderr: String::new() })
    }
}

/// time.sleep patched out; monotonic real.
struct Quick;

impl Clock for Quick {
    fn monotonic(&self) -> f64 {
        SystemClock.monotonic()
    }

    fn sleep(&self, _secs: f64) {}
}

/// time.monotonic patched to count 0, 1, 2, ... and time.sleep patched out.
#[derive(Default)]
struct Counting(AtomicU64);

impl Clock for Counting {
    fn monotonic(&self) -> f64 {
        self.0.fetch_add(1, Ordering::SeqCst) as f64
    }

    fn sleep(&self, _secs: f64) {}
}

fn camera(h: &Helper) -> UsbCamera {
    UsbCamera::new().with_runner(h.clone()).with_helper("/x").with_clock(Quick).with_waker(|| {})
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn test_read_and_asleep_by_the_parked_tilt() {
    let mut awake = camera(&Helper::new(3.0, ""));
    assert_eq!(awake.read().unwrap()["asleep"], json!(false));
    let mut asleep = camera(&Helper::new(-79.0, ""));
    assert_eq!(asleep.read().unwrap()["asleep"], json!(true));
}

#[test]
fn test_read_keeps_the_helpers_fields_in_order() {
    let r = camera(&Helper::new(3.0, "")).read().unwrap();
    let keys: Vec<&str> = r.keys().map(String::as_str).collect();
    assert_eq!(keys, ["ok", "name", "pan", "tilt", "zoom", "zoom_max", "asleep"]);
    assert_eq!(r["name"], json!("OBSBOT Tiny 3"));
}

#[test]
fn test_no_camera_is_a_plain_line() {
    let mut cam = camera(&Helper::new(0.0, "no camera"));
    assert!(cam.read().is_none());
    assert_eq!(cam.why, NO_CAMERA_LINE);
    assert_eq!(UsbControl::why(&cam), NO_CAMERA_LINE);
}

#[test]
fn test_other_helper_errors_are_passed_through() {
    let mut cam = camera(&Helper::new(0.0, "pantilt refused"));
    assert!(!cam.point(1.0, 2.0));
    assert_eq!(cam.why, "pantilt refused");
}

#[test]
fn test_turn_directions_and_speed() {
    let h = Helper::default();
    let mut cam = camera(&h);
    for d in ["left", "right", "up", "down", "stop"] {
        assert!(cam.turn(d.parse().unwrap(), TURN_SPEED));
    }
    let speed = TURN_SPEED.to_string();
    let s = speed.as_str();
    assert_eq!(
        h.argv(),
        vec![
            strs(&["turn", "-1", "0", s]),
            strs(&["turn", "1", "0", s]),
            strs(&["turn", "0", "1", s]),
            strs(&["turn", "0", "-1", s]),
            strs(&["turn", "0", "0", s]),
        ]
    );
}

#[test]
fn test_unknown_direction_is_refused() {
    assert_eq!("sideways".parse::<Direction>(), Err(UnknownDirection("sideways".into())));
    assert_eq!(Direction::Left.to_string(), "left");
}

#[test]
fn test_point_and_zoom_arguments() {
    let h = Helper::default();
    let mut cam = camera(&h);
    assert!(cam.point(20.0, -10.5) && cam.zoom(40));
    assert_eq!(h.argv(), vec![strs(&["pantilt", "20.00", "-10.50"]), strs(&["zoom", "40"])]);
}

#[test]
fn test_point_formats_like_python() {
    // f"{x:.2f}": round-half-even on the binary value, a signed zero, and "nan".
    assert_eq!(fmt2(0.125), "0.12");
    assert_eq!(fmt2(2.675), "2.67");
    assert_eq!(fmt2(-0.001), "-0.00");
    assert_eq!(fmt2(f64::NAN), "nan");
    assert_eq!(fmt2(f64::NEG_INFINITY), "-inf");
}

#[test]
fn test_wake_opens_the_video_only_when_asleep() {
    let opened = Arc::new(Mutex::new(Vec::<i32>::new()));
    let h = Helper::new(-79.0, "");
    let (o, hh) = (opened.clone(), h.clone());
    let mut asleep = camera(&h).with_waker(move || {
        o.lock().push(1);
        hh.0.lock().tilt = 0.0; // the gimbal unparks once the video opens
    });
    assert!(asleep.wake() && *opened.lock() == vec![1]);
    let o = opened.clone();
    let mut awake = camera(&Helper::new(0.0, "")).with_waker(move || o.lock().push(2));
    assert!(awake.wake() && *opened.lock() == vec![1]);
}

#[test]
fn test_wake_gives_up_when_the_camera_stays_parked() {
    let h = Helper::new(-79.0, "");
    let mut cam = camera(&h).with_clock(Counting::default());
    assert!(!cam.wake());
    // one read before the waker, then one per tick until the 6 s budget runs out
    assert_eq!(h.argv().len(), 1 + 5);
}

#[test]
fn test_wake_fails_when_the_camera_cannot_be_read() {
    let mut cam = camera(&Helper::new(0.0, "no camera"));
    assert!(!cam.wake());
}

fn fake_clang(calls: Arc<Mutex<Vec<Vec<String>>>>) -> impl Runner {
    move |argv: &[String], _t: Duration| -> io::Result<RunOutput> {
        calls.lock().push(argv.to_vec());
        let i = argv.iter().position(|a| a == "-o").unwrap();
        std::fs::write(&argv[i + 1], "binary")?;
        // the source clang reads is the embedded helper source, on disk for the call
        assert_eq!(std::fs::read(argv.last().unwrap())?, SOURCE_BYTES);
        Ok(RunOutput::default())
    }
}

#[test]
fn test_build_compiles_once_per_source() {
    let tmp = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(vec![]));
    let clang = fake_clang(calls.clone());
    std::fs::write(tmp.path().join("obsbot-uvc-old"), "stale").unwrap();
    let (path, why) = build_in(tmp.path(), Some(Path::new("/usr/bin/clang")), &clang);
    let path = path.unwrap();
    assert_eq!(why, "");
    assert_eq!(path, helper_path_in(tmp.path()));
    assert!(path.exists());
    assert!(!tmp.path().join("obsbot-uvc-old").exists()); // an older build is cleared
    assert_eq!(build_in(tmp.path(), Some(Path::new("/usr/bin/clang")), &clang).0, Some(path)); // built once
    assert_eq!(calls.lock().len(), 1);
    let argv = calls.lock()[0].clone();
    assert_eq!(&argv[..7], &strs(&["/usr/bin/clang", "-O2", "-framework", "IOKit", "-framework", "CoreFoundation", "-o"])[..]);
    // only the binary is left: no tmp, no written source
    let left: Vec<String> =
        std::fs::read_dir(tmp.path()).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(left, vec![helper_path_in(tmp.path()).file_name().unwrap().to_string_lossy().into_owned()]);
}

#[test]
fn test_helper_name_is_the_source_hash_python_uses() {
    let full = hex::encode(Sha256::digest(std::fs::read(SOURCE).unwrap()));
    let name = helper_path_in(Path::new("/b")).file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(name, format!("obsbot-uvc-{}", &full[..10]));
    assert!(helper_path().starts_with(bin_dir()));
    assert!(bin_dir().ends_with(".config/eve-bridge/bin"));
}

#[test]
fn test_build_failure_is_a_reason() {
    let tmp = tempfile::tempdir().unwrap();
    let broken = |_argv: &[String], _t: Duration| -> io::Result<RunOutput> {
        Ok(RunOutput { returncode: 1, stdout: String::new(), stderr: "error: no IOKit".into() })
    };
    let (path, why) = build_in(tmp.path(), Some(Path::new("/usr/bin/clang")), &broken);
    assert!(path.is_none() && why.contains("clang failed"));
}

#[test]
fn test_build_without_clang_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let never = |_argv: &[String], _t: Duration| -> io::Result<RunOutput> { panic!("clang was run") };
    assert_eq!(build_in(tmp.path(), None, &never), (None, NO_CLANG_LINE.to_string()));
}

#[test]
fn test_call_builds_the_helper_and_reports_why() {
    let tmp = tempfile::tempdir().unwrap();
    let broken = |_argv: &[String], _t: Duration| -> io::Result<RunOutput> {
        Ok(RunOutput { returncode: 1, stdout: String::new(), stderr: "boom".into() })
    };
    let mut cam = UsbCamera::new().with_runner(broken).with_bin_dir(tmp.path());
    assert!(cam.read().is_none());
    // either no clang on this machine, or clang "failed": both name the reason
    assert!(cam.why == NO_CLANG_LINE || cam.why.contains("clang failed"), "{}", cam.why);
}

#[test]
fn test_helper_failures_name_the_python_exception() {
    let cases: Vec<(Box<dyn Runner>, &str)> = vec![
        (Box::new(|_a: &[String], _t: Duration| Ok(RunOutput::default())), "IndexError"),
        (
            Box::new(|_a: &[String], _t: Duration| {
                Ok(RunOutput { returncode: 0, stdout: "garbage\n".into(), stderr: String::new() })
            }),
            "JSONDecodeError",
        ),
        (
            Box::new(|_a: &[String], _t: Duration| Err(io::Error::new(io::ErrorKind::TimedOut, "slow"))),
            "TimeoutExpired",
        ),
        (
            Box::new(|_a: &[String], _t: Duration| Err(io::Error::new(io::ErrorKind::NotFound, "gone"))),
            "FileNotFoundError",
        ),
    ];
    for (runner, name) in cases {
        let mut cam = UsbCamera { run: runner, ..UsbCamera::new() }.with_helper("/x");
        let r = cam.call(&["get".into()]);
        assert_eq!(r["error"], json!(format!("the camera helper failed ({name})")));
        assert_eq!(cam.why, ""); // a crashed helper leaves why alone, as in Python
    }
}

#[test]
fn test_reply_is_the_last_line() {
    let r = |_a: &[String], _t: Duration| {
        Ok(RunOutput { returncode: 0, stdout: "noise\r\n{\"ok\":true}\n\n".into(), stderr: String::new() })
    };
    let mut cam = UsbCamera::new().with_runner(r).with_helper("/x");
    assert!(cam.zoom(3));
}

#[test]
fn test_present_reads_the_usb_bus() {
    let ioreg = |found: bool| {
        move |argv: &[String], _t: Duration| -> io::Result<RunOutput> {
            assert_eq!(argv, &strs(&["ioreg", "-p", "IOUSB", "-l", "-w0"])[..]);
            let stdout = if found { "\"idVendor\" = 13668\n" } else { "\"idVendor\" = 1452\n" };
            Ok(RunOutput { returncode: 0, stdout: stdout.into(), stderr: String::new() })
        }
    };
    assert!(present(&ioreg(true)));
    assert!(!present(&ioreg(false)));
    let missing = |_a: &[String], _t: Duration| -> io::Result<RunOutput> { Err(io::ErrorKind::NotFound.into()) };
    assert!(!present(&missing));
}

#[test]
fn test_video_device_by_name() {
    let listing = "[AVFoundation indev @ 0x1] AVFoundation video devices:\n\
                   [AVFoundation indev @ 0x1] [0] FaceTime HD Camera\n\
                   [AVFoundation indev @ 0x1] [2] OBSBOT Tiny 3\n\
                   [AVFoundation indev @ 0x1] AVFoundation audio devices:\n";
    assert_eq!(video_device_in(listing), "2");
    let audio_only = "AVFoundation video devices:\n[0] FaceTime\nAVFoundation audio devices:\n[1] OBSBOT Tiny 3 Mic\n";
    assert_eq!(video_device_in(audio_only), "0");
    assert_eq!(video_device_in(""), "0");
}

#[test]
fn test_python_string_helpers() {
    assert_eq!(py_splitlines("a\r\nb\rc\n"), vec!["a", "b", "c"]);
    assert_eq!(py_splitlines(""), Vec::<&str>::new());
    assert_eq!(last_chars("héllo", 3), "llo");
    assert_eq!(py_str(&json!(1.5)), "1.5");
    assert_eq!(py_str(&json!(1e20)), "1e+20");
    assert_eq!(py_str(&json!(true)), "True");
    assert_eq!(py_float(&json!("-79.0")), Some(-79.0));
}

#[test]
fn test_the_helper_source_builds_with_this_macs_clang() {
    // The real helper compiles (it is C against IOKit): a broken edit fails here, not on the owner's first move.
    let Some(clang) = find_clang() else {
        eprintln!("skipped: no clang on this machine");
        return;
    };
    if !cfg!(target_os = "macos") {
        eprintln!("skipped: the helper is IOKit C");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let h = tmp.path().join("h");
    let out = Command::new(clang)
        .args(["-O2", "-Wall", "-Werror", "-framework", "IOKit", "-framework", "CoreFoundation", "-o"])
        .arg(&h)
        .arg(SOURCE)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let usage = Command::new(&h).output().unwrap();
    let reply: Value = serde_json::from_slice(&usage.stdout).unwrap();
    assert_eq!(reply["ok"], json!(false)); // no arguments: usage, never a crash
}

#[test]
fn test_build_for_real_and_read_without_a_camera() {
    // The whole path with a real clang: build into a temp bin dir, run the helper; with no OBSBOT plugged in the
    // owner sees the plain line, with one plugged in the read succeeds.
    if find_clang().is_none() || !cfg!(target_os = "macos") {
        eprintln!("skipped: needs clang on a Mac");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let mut cam = UsbCamera::new().with_bin_dir(tmp.path());
    match cam.read() {
        Some(r) => assert!(r.contains_key("asleep")),
        None => assert_eq!(cam.why, NO_CAMERA_LINE),
    }
    assert!(helper_path_in(tmp.path()).exists());
}
