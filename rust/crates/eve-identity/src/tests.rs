//! Port of `bridge/tests/test_identity.py` (the identity half; the Daemon
//! tests belong to the eve-daemon port), plus parity checks.
//!
//! No Vision framework here: prints are short fake vectors, distance is plain
//! Euclidean (what Vision computes anyway).

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use eve_vision::{FaceTracker, Frame, Rect, Sender};
use serde_json::{json, Value};

use super::*;

const W: i64 = 160;
const H: i64 = 120;
const OWNER: [f64; 3] = [1.0, 0.0, 0.0];
const OWNER_NEAR: [f64; 3] = [0.9, 0.1, 0.0]; // d ≈ 0.14 from OWNER
const STRANGER: [f64; 3] = [0.0, 1.0, 0.0]; // d ≈ 1.41 from OWNER

fn big() -> Rect {
    Rect::new(40.0, 20.0, 48.0, 48.0) // 30 % of frame width
}

fn small() -> Rect {
    Rect::new(70.0, 50.0, 16.0, 16.0) // 10 %
}

fn core() -> OwnerIdentity {
    OwnerIdentity::new().with_threshold(0.5)
}

fn core_at(path: &Path) -> OwnerIdentity {
    core().with_path(path)
}

fn frame() -> Frame {
    Frame::new(1, W, H, "gray", vec![0u8; (W * H) as usize])
}

fn gray_obj(seq: i64) -> Value {
    use base64::Engine as _;
    json!({"seq": seq, "w": W, "h": H, "fmt": "gray",
           "b64": base64::engine::general_purpose::STANDARD.encode(vec![0u8; (W * H) as usize])})
}

fn approx(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 * b.abs().max(1e-12)
}

fn d(a: &[f64], b: &[f64]) -> f64 {
    euclidean(a, b).unwrap()
}

/// Runs `f` with a subscriber that captures each log message as one line (caplog).
fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, String) {
    #[derive(Clone)]
    struct Buf(Arc<StdMutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf(Arc::new(StdMutex::new(Vec::new())));
    let writer = buf.clone();
    let sub = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .without_time()
        .with_level(false)
        .with_target(false)
        .finish();
    let r = tracing::subscriber::with_default(sub, f);
    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    (r, text)
}

type Now = Arc<StdMutex<f64>>;
type Down = Arc<StdMutex<bool>>;

fn driver(core: Arc<OwnerIdentity>, vec: Option<Vec<f64>>, listen_down: bool) -> (FaceIdentity, Down, Now) {
    let now: Now = Arc::new(StdMutex::new(100.0));
    let down: Down = Arc::new(StdMutex::new(listen_down));
    let (n2, d2) = (now.clone(), down.clone());
    let describe: Describer = Arc::new(move |_, _| Ok(vec.clone()));
    let fi = FaceIdentity::new(core, describe)
        .with_listen_down(Arc::new(move || *d2.lock().unwrap()))
        .with_clock(Arc::new(move || *n2.lock().unwrap()));
    (fi, down, now)
}

// ---- core: classify / enrol / replace --------------------------------------

#[test]
fn test_empty_store_is_unknown() {
    assert_eq!(core().classify(&OWNER).unwrap(), ("unknown", None));
}

#[test]
fn test_classify_by_min_distance_against_threshold() {
    let c = core();
    c.enrol(&OWNER, None).unwrap();
    let (who, dist) = c.classify(&OWNER_NEAR).unwrap();
    assert!(who == "owner" && approx(dist.unwrap(), d(&OWNER, &OWNER_NEAR)));
    let (who, dist) = c.classify(&STRANGER).unwrap();
    assert!(who == "unknown" && approx(dist.unwrap(), d(&OWNER, &STRANGER)));
}

#[test]
fn test_threshold_boundary_is_exclusive() {
    let c = OwnerIdentity::new().with_threshold(1.0);
    c.enrol(&[0.0, 0.0], None).unwrap();
    assert_eq!(c.classify(&[1.0, 0.0]).unwrap().0, "unknown"); // d == threshold
    assert_eq!(c.classify(&[0.99, 0.0]).unwrap().0, "owner");
}

#[test]
fn test_enrol_returns_slot_and_copies_vector() {
    let c = core();
    let mut vec = vec![1.0, 2.0];
    assert_eq!(c.enrol(&vec, None).unwrap(), 1);
    vec[0] = 99.0;
    assert_eq!(c.prints(), vec![vec![1.0, 2.0]]);
    assert_eq!(c.enrol(&[3.0, 4.0], None).unwrap(), 2);
    assert_eq!(c.len(), 2);
}

#[test]
fn test_full_store_replaces_the_print_nearest_to_the_new_one() {
    let c = core().with_capacity(3);
    c.enrol(&[0.0, 0.0], None).unwrap();
    c.enrol(&[10.0, 0.0], None).unwrap();
    c.enrol(&[20.0, 0.0], None).unwrap();
    let slot = c.enrol(&[10.5, 0.0], None).unwrap(); // nearest is slot 2
    assert_eq!(slot, 2);
    assert_eq!(c.prints(), vec![vec![0.0, 0.0], vec![10.5, 0.0], vec![20.0, 0.0]]);
    assert_eq!(c.len(), 3);
}

#[test]
fn test_default_capacity_is_24() {
    assert_eq!(CAPACITY, 24);
    let c = core();
    for i in 0..30 {
        c.enrol(&[i as f64], None).unwrap();
    }
    assert_eq!(c.len(), 24);
}

#[test]
fn test_reset_clears_memory_and_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("owner.json");
    let c = core_at(&path);
    c.enrol(&OWNER, None).unwrap();
    c.save(None).unwrap();
    assert!(path.exists());
    assert_eq!(c.reset().unwrap(), 1);
    assert!(c.is_empty() && c.enrolled_at().is_none());
    assert!(!path.exists());
    assert_eq!(c.reset().unwrap(), 0); // idempotent, no file
}

// ---- persistence ------------------------------------------------------------

#[test]
fn test_save_load_round_trip_mode_600() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("cfg").join("owner_faceprints.json");
    let c = core_at(&path);
    c.enrol(&OWNER, Some(1000.0)).unwrap();
    c.enrol(&STRANGER, Some(1001.0)).unwrap();
    c.save(None).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert!(raw["version"] == 1 && raw["revision"] == 2);
    assert_eq!(raw["prints"], json!([OWNER, STRANGER]));
    assert_eq!(raw["enrolled_at"], json!(1001.0));

    let d = core_at(&path);
    assert_eq!(d.load(None), 2);
    assert_eq!(d.prints(), vec![OWNER.to_vec(), STRANGER.to_vec()]);
    assert_eq!(d.enrolled_at(), Some(1001.0));
    assert_eq!(d.classify(&OWNER_NEAR).unwrap().0, "owner");
}

#[test]
fn test_load_missing_file_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let c = core_at(&tmp.path().join("nope.json"));
    assert_eq!(c.load(None), 0);
    assert!(c.prints().is_empty());
}

#[test]
fn test_load_rejects_malformed_and_keeps_going() {
    let cases = [
        "not json".to_string(),
        "[]".to_string(),
        json!({"version": 2, "revision": 2, "prints": []}).to_string(),
        json!({"version": 1, "revision": 1, "prints": [[1.0]]}).to_string(),
        json!({"version": 1, "revision": 2, "prints": "x"}).to_string(),
        json!({"version": 1, "revision": 2, "prints": [[1.0], [1.0, 2.0]]}).to_string(),
        json!({"version": 1, "revision": 2, "prints": [["a"]]}).to_string(),
        json!({"version": 1, "revision": 2, "prints": [[]]}).to_string(),
    ];
    for raw in cases {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("owner.json");
        std::fs::write(&path, &raw).unwrap();
        let c = core_at(&path);
        c.enrol(&OWNER, None).unwrap();
        let (n, logs) = capture_logs(|| c.load(None));
        assert_eq!(n, 0, "{raw}");
        assert!(logs.contains("identity: ignoring"), "{raw}: {logs}");
        assert_eq!(c.prints(), vec![OWNER.to_vec()], "{raw}"); // a bad file does not wipe memory
    }
}

#[test]
fn test_default_path_env_override() {
    assert_eq!(default_path_from(None, Some("/x/cfg")), PathBuf::from("/x/cfg/eve-bridge/owner_faceprints.json"));
    let home = eve_vision::expanduser("~");
    assert_eq!(default_path_from(None, None), home.join(".config").join("eve-bridge").join("owner_faceprints.json"));
    assert_eq!(default_path_from(Some("~/mine.json"), Some("/x/cfg")), home.join("mine.json"));
    // Empty values count as unset, as Python's truthiness does.
    assert_eq!(default_path_from(Some(""), Some("")), default_path_from(None, None));
}

#[test]
fn test_configured_threshold() {
    assert_eq!(configured_threshold_from(None), DEFAULT_THRESHOLD);
    assert_eq!(DEFAULT_THRESHOLD, 0.9);
    assert_eq!(configured_threshold_from(Some("0.75")), 0.75);
    for bad in ["abc", "-1", "0", "nan"] {
        assert_eq!(configured_threshold_from(Some(bad)), DEFAULT_THRESHOLD, "{bad}");
    }
}

// ---- enrolment policy -------------------------------------------------------

#[test]
fn test_gate_requires_key_one_face_and_size() {
    let g = EnrolmentGate::default();
    assert!(g.allows(true, 1, 20.0, 0.0));
    assert!(!g.allows(false, 1, 30.0, 0.0)); // key up
    assert!(!g.allows(true, 2, 30.0, 0.0)); // two faces
    assert!(!g.allows(true, 0, 30.0, 0.0)); // no face
    assert!(!g.allows(true, 1, 19.9, 0.0)); // too small
}

#[test]
fn test_gate_one_enrolment_per_interval() {
    let mut g = EnrolmentGate::default();
    assert!(g.allows(true, 1, 30.0, 10.0));
    g.mark(10.0);
    assert!(!g.allows(true, 1, 30.0, 11.9));
    assert!(g.allows(true, 1, 30.0, 12.0));
}

// ---- FaceIdentity driver ----------------------------------------------------

#[test]
fn test_process_enrols_only_while_key_down_with_one_big_face() {
    let tmp = tempfile::tempdir().unwrap();
    let core = Arc::new(core_at(&tmp.path().join("owner.json")));
    let (fi, down, _) = driver(core.clone(), Some(OWNER.to_vec()), false);
    let (_, logs) = capture_logs(|| {
        assert_eq!(fi.process(&frame(), &[big()]).unwrap(), "unknown"); // key up: no enrol
        assert_eq!(core.len(), 0);
        *down.lock().unwrap() = true;
        assert_eq!(fi.process(&frame(), &[big(), small()]).unwrap(), "unknown"); // two faces
        assert_eq!(fi.process(&frame(), &[small()]).unwrap(), "unknown"); // too small
        assert_eq!(core.len(), 0);
        assert_eq!(fi.process(&frame(), &[big()]).unwrap(), "owner"); // enrolled, then matched
    });
    assert!(core.len() == 1 && fi.enrolments() == 1);
    assert!(logs.contains("identity: enrolled owner print #1 (size=30%, 1/24 held)"), "{logs}");
    assert!(tmp.path().join("owner.json").exists()); // persisted on enrolment
}

#[test]
fn test_process_rate_limits_enrolment_to_one_per_2s() {
    let tmp = tempfile::tempdir().unwrap();
    let core = Arc::new(core_at(&tmp.path().join("owner.json")));
    let (fi, _, now) = driver(core.clone(), Some(OWNER.to_vec()), true);
    fi.process(&frame(), &[big()]).unwrap();
    *now.lock().unwrap() += 1.0;
    fi.process(&frame(), &[big()]).unwrap();
    assert_eq!(core.len(), 1);
    *now.lock().unwrap() += 1.0;
    fi.process(&frame(), &[big()]).unwrap();
    assert_eq!(core.len(), 2);
}

#[test]
fn test_process_no_face_or_no_print_is_unknown() {
    let core = Arc::new(core());
    core.enrol(&OWNER, None).unwrap();
    let (fi, _, _) = driver(core, None, false);
    assert_eq!(fi.process(&frame(), &[]).unwrap(), "unknown");
    assert_eq!(fi.process(&frame(), &[big()]).unwrap(), "unknown");
    assert!(fi.last_who().is_none());
}

#[test]
fn test_process_describer_failure_is_logged_not_raised() {
    let boom: Describer = Arc::new(|_, _| anyhow::bail!("vision down"));
    let fi = FaceIdentity::new(Arc::new(core()), boom);
    let (who, logs) = capture_logs(|| fi.process(&frame(), &[big()]).unwrap());
    assert_eq!(who, "unknown");
    assert!(logs.contains("feature print failed"), "{logs}");
}

#[test]
fn test_process_describer_panic_is_logged_not_raised() {
    let boom: Describer = Arc::new(|_, _| panic!("vision down"));
    let fi = FaceIdentity::new(Arc::new(core()), boom);
    let (who, logs) = capture_logs(|| fi.process(&frame(), &[big()]).unwrap());
    assert_eq!(who, "unknown");
    assert!(logs.contains("identity: feature print failed on frame 1"), "{logs}");
}

#[test]
fn test_transition_logs_are_rate_limited() {
    let core = Arc::new(core());
    core.enrol(&OWNER, None).unwrap();
    let seen = Arc::new(StdMutex::new(STRANGER.to_vec()));
    let now: Now = Arc::new(StdMutex::new(0.0));
    let (s2, n2) = (seen.clone(), now.clone());
    let fi = FaceIdentity::new(core, Arc::new(move |_, _| Ok(Some(s2.lock().unwrap().clone()))))
        .with_clock(Arc::new(move || *n2.lock().unwrap()));
    let set = |v: [f64; 3], t: f64| {
        *seen.lock().unwrap() = v.to_vec();
        *now.lock().unwrap() = t;
    };
    let (_, logs) = capture_logs(|| {
        fi.process(&frame(), &[big()]).unwrap(); // unknown: first classification logs
        fi.process(&frame(), &[big()]).unwrap(); // same: silent
        set(OWNER_NEAR, 10.0);
        fi.process(&frame(), &[big()]).unwrap(); // owner, inside 30 s: suppressed
        set(STRANGER, 31.0);
        fi.process(&frame(), &[big()]).unwrap(); // unknown again, window passed: logs
        set(OWNER_NEAR, 62.0);
        fi.process(&frame(), &[big()]).unwrap(); // owner: logs
    });
    let lines: Vec<&str> = logs.lines().map(str::trim).collect();
    assert_eq!(
        lines,
        vec![
            "identity: unknown face (d=1.41)",
            "identity: unknown face (d=1.41)",
            "identity: owner recognised (d=0.14)",
        ]
    );
    assert_eq!(fi.last_who().as_deref(), Some("owner"));
}

#[test]
fn test_status_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let core = Arc::new(core_at(&tmp.path().join("o.json")).with_threshold(0.7));
    let (fi, _, _) = driver(core, Some(OWNER.to_vec()), true);
    fi.process(&frame(), &[big()]).unwrap();
    let st = fi.status();
    assert!(st["prints"] == 1 && st["capacity"] == 24 && st["threshold"] == 0.7);
    assert_eq!(st["path"], json!(tmp.path().join("o.json").to_string_lossy()));
    assert!(st["last_who"] == "owner" && st["last_distance"] == 0.0);
    assert!(st["enrolments"] == 1 && st["enrolled_at"].is_f64());
    let keys: Vec<&String> = st.as_object().unwrap().keys().collect();
    assert_eq!(
        keys,
        ["prints", "capacity", "threshold", "path", "enrolled_at", "last_who", "last_distance", "enrolments"]
    );
}

// ---- crop geometry ----------------------------------------------------------

#[test]
fn test_crop_rect_pads_25_percent_and_clamps() {
    assert_eq!(crop_rect(&Rect::new(40.0, 40.0, 40.0, 40.0), 160.0, 120.0), (30, 30, 60, 60));
    assert_eq!(crop_rect(&Rect::new(0.0, 0.0, 40.0, 40.0), 160.0, 120.0), (0, 0, 50, 50)); // top-left clamp
    assert_eq!(crop_rect(&Rect::new(130.0, 90.0, 40.0, 40.0), 160.0, 120.0), (120, 80, 40, 40)); // bottom-right clamp
}

// ---- tracker wire edge --------------------------------------------------------

struct FakeIdentity {
    who: &'static str,
    calls: StdMutex<Vec<(i64, usize)>>,
}

impl eve_vision::Identity for FakeIdentity {
    fn process(&self, frame: &Frame, rects: &[Rect]) -> anyhow::Result<String> {
        self.calls.lock().unwrap().push((frame.seq, rects.len()));
        Ok(self.who.to_string())
    }
}

fn sender() -> (Sender, Arc<StdMutex<Vec<Value>>>) {
    let sent = Arc::new(StdMutex::new(Vec::new()));
    let s2 = sent.clone();
    let send: Sender = Arc::new(move |obj| {
        s2.lock().unwrap().push(obj);
        Box::pin(async { true })
    });
    (send, sent)
}

async fn sleep(secs: f64) {
    tokio::time::sleep(std::time::Duration::from_secs_f64(secs)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tracker_puts_identity_answer_in_who() {
    let fake = Arc::new(FakeIdentity { who: "owner", calls: StdMutex::new(Vec::new()) });
    let (send, sent) = sender();
    let detect: eve_vision::Detector = Arc::new(|f: &Frame| {
        Ok(if f.seq == 1 { vec![Rect::with_conf(60.0, 40.0, 40.0, 40.0, 1.0)] } else { vec![] })
    });
    let t = FaceTracker::with_options(
        Some(detect),
        send,
        None,
        Arc::new(eve_vision::monotonic),
        Some(fake.clone() as Arc<dyn eve_vision::Identity>),
        None,
    );
    t.on_frame(&gray_obj(1)).await;
    sleep(0.05).await;
    t.on_frame(&gray_obj(2)).await;
    sleep(0.05).await;
    t.stop();
    assert_eq!(
        *sent.lock().unwrap(),
        vec![
            json!({"cmd": "face", "seq": 1, "bx": 0, "by": 0, "size": 25, "conf": 100, "who": "owner"}),
            json!({"cmd": "face", "seq": 2, "conf": 0, "who": "unknown"}),
        ]
    );
    assert_eq!(*fake.calls.lock().unwrap(), vec![(1, 1)]); // identity never runs without a face
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_tracker_runs_the_real_face_identity() {
    let core = Arc::new(core());
    core.enrol(&OWNER, None).unwrap();
    let fi: Arc<dyn eve_vision::Identity> =
        Arc::new(FaceIdentity::new(core, Arc::new(|_, _| Ok(Some(OWNER_NEAR.to_vec())))));
    let (send, sent) = sender();
    let detect: eve_vision::Detector = Arc::new(|_: &Frame| Ok(vec![Rect::new(60.0, 40.0, 40.0, 40.0)]));
    let t = FaceTracker::with_options(Some(detect), send, None, Arc::new(eve_vision::monotonic), Some(fi), None);
    t.on_frame(&gray_obj(3)).await;
    sleep(0.05).await;
    t.stop();
    assert_eq!(sent.lock().unwrap()[0]["who"], "owner");
}

#[cfg(not(target_os = "macos"))]
#[test]
fn test_make_describer_none_off_darwin() {
    let (d, logs) = capture_logs(make_describer);
    assert!(d.is_none());
    assert!(logs.contains("needs macOS Vision"), "{logs}");
}

#[cfg(target_os = "macos")]
#[test]
fn test_make_describer_some_on_darwin() {
    assert!(make_describer().is_some());
}

/// `tests/test_vision_native.py::test_native_identity_request_accepts_default_options`.
#[cfg(target_os = "macos")]
#[test]
fn test_native_identity_request_accepts_default_options() {
    let frame = Frame::new(1, 160, 120, "gray", vec![128u8; 160 * 120]);
    let vector = vision_feature_print(&frame, &Rect::new(20.0, 20.0, 80.0, 80.0)).unwrap().unwrap();
    assert!(!vector.is_empty() && vector.iter().all(|v| v.is_finite()));
}

#[cfg(target_os = "macos")]
#[test]
fn test_native_identity_tiny_crop_is_none() {
    // A crop under 2 px on a side gives no print, as in Python.
    let frame = Frame::new(1, 160, 120, "gray", vec![128u8; 160 * 120]);
    assert!(vision_feature_print(&frame, &Rect::new(159.5, 10.0, 0.2, 10.0)).unwrap().is_none());
}

// ---- parity checks beyond the Python tests -----------------------------------

#[test]
fn test_euclidean_sums_like_cpython() {
    // CPython 3.12+ sums floats with Neumaier compensation; a naive sum gives 1e8.
    assert_eq!(d(&[1e8, 1.0, 1.0], &[0.0, 0.0, 0.0]), 100000000.00000001);
    assert_eq!(d(&[0.1, 0.2, 0.3], &[0.3, 0.1, 0.7]), 0.45825756949558394);
    assert_eq!(d(&[], &[]), 0.0);
    assert_eq!(euclidean(&[1.0], &[1.0, 2.0]).unwrap_err().to_string(), "print length mismatch: 1 vs 2");
}

#[test]
fn test_classify_length_mismatch_propagates() {
    let c = core();
    c.enrol(&[1.0, 0.0], None).unwrap();
    assert!(c.classify(&OWNER).is_err());
    let fi = FaceIdentity::new(Arc::new(c), Arc::new(|_, _| Ok(Some(OWNER.to_vec()))));
    assert!(fi.process(&frame(), &[big()]).is_err());
}

#[test]
fn test_saved_bytes_match_python_json_dump() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("o.json");
    let c = core_at(&path);
    c.enrol(&OWNER, Some(1000.0)).unwrap();
    c.enrol(&[0.0, 1e-05, 1e16], Some(1001.5)).unwrap();
    assert_eq!(c.save(None).unwrap(), path);
    // Written by CPython: json.dumps(snapshot, separators=(",", ":")).
    assert_eq!(
        std::fs::read_to_string(&path).unwrap(),
        r#"{"version":1,"revision":2,"threshold":0.5,"enrolled_at":1001.5,"prints":[[1.0,0.0,0.0],[0.0,1e-05,1e+16]]}"#
    );
    assert!(!tmp.path().join("o.json.tmp").exists());
    let fresh = core();
    fresh.enrol(&OWNER, None).unwrap();
    let other = tmp.path().join("other.json");
    fresh.save(Some(&other)).unwrap();
    let raw: Value = serde_json::from_str(&std::fs::read_to_string(&other).unwrap()).unwrap();
    assert_eq!(raw, fresh.snapshot());
    assert!(core().save(None).is_err()); // no path to save to
}

#[test]
fn test_load_takes_what_python_float_takes() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("o.json");
    // 1 == 1.0 == True for the version; float() takes numeric strings and bools.
    std::fs::write(
        &path,
        r#"{"version":true,"revision":2.0,"enrolled_at":false,"prints":[[" 1_0.5 ",true,3],[1,2,3]]}"#,
    )
    .unwrap();
    let c = core_at(&path);
    assert_eq!(c.load(None), 2);
    assert_eq!(c.prints(), vec![vec![10.5, 1.0, 3.0], vec![1.0, 2.0, 3.0]]);
    assert_eq!(c.enrolled_at(), Some(0.0));
}

#[test]
fn test_load_truncates_to_capacity_and_logs_python_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("o.json");
    let prints: Vec<Vec<f64>> = (0..5).map(|i| vec![i as f64]).collect();
    std::fs::write(&path, json!({"version": 1, "revision": 2, "prints": prints}).to_string()).unwrap();
    assert_eq!(core_at(&path).with_capacity(3).load(None), 3);

    let cases = [
        (json!({"version": 1, "revision": 2, "prints": [[null]]}).to_string(),
         "float() argument must be a string or a real number, not 'NoneType'"),
        (json!({"version": 1, "revision": 2, "prints": [["a"]]}).to_string(), "could not convert string to float: 'a'"),
        (json!({"version": 1, "revision": 2, "prints": [["inf"]]}).to_string(), "print holds a non-finite value"),
        (json!({"version": 1, "revision": 1}).to_string(), "prints are revision 1, want 2 — reset"),
        (json!({"version": "1"}).to_string(), "unsupported version '1'"),
        ("{}".to_string(), "unsupported version None"),
    ];
    for (raw, msg) in cases {
        std::fs::write(&path, &raw).unwrap();
        let (n, logs) = capture_logs(|| core_at(&path).load(None));
        assert_eq!(n, 0);
        assert!(logs.contains(&format!("identity: ignoring {}: {msg}", path.display())), "{raw}: {logs}");
    }
    std::fs::write(&path, b"\xff").unwrap();
    let (_, logs) = capture_logs(|| core_at(&path).load(None));
    assert!(logs.contains("'utf-8' codec can't decode byte 0xff in position 0: invalid start byte"), "{logs}");
}

#[test]
fn test_py_float_str_like_python() {
    assert_eq!(py_float_str(" 1_0.5 "), Some(10.5));
    assert_eq!(py_float_str(".5"), Some(0.5));
    assert_eq!(py_float_str("5."), Some(5.0));
    assert_eq!(py_float_str("1e1_0"), Some(1e10));
    assert_eq!(py_float_str("-Infinity"), Some(f64::NEG_INFINITY));
    assert!(py_float_str("+NaN").unwrap().is_nan());
    for bad in ["1__0", "_1", "1_", "1_e5", "0x1", "1.5e", "", "abc"] {
        assert_eq!(py_float_str(bad), None, "{bad}");
    }
}

#[test]
fn test_save_failure_is_logged_once_and_classification_goes_on() {
    let tmp = tempfile::tempdir().unwrap();
    let blocker = tmp.path().join("file");
    std::fs::write(&blocker, "x").unwrap();
    // The parent "directory" is a file, so every save fails.
    let core = Arc::new(core_at(&blocker.join("owner.json")));
    let (fi, _, now) = driver(core.clone(), Some(OWNER.to_vec()), true);
    let (_, logs) = capture_logs(|| {
        assert_eq!(fi.process(&frame(), &[big()]).unwrap(), "owner");
        *now.lock().unwrap() += 5.0;
        assert_eq!(fi.process(&frame(), &[big()]).unwrap(), "owner");
    });
    assert_eq!(core.len(), 2);
    assert_eq!(logs.matches("identity: could not save").count(), 1, "{logs}");
    assert!(logs.contains("[Errno 17] File exists: "), "{logs}");
}

#[test]
fn test_full_store_with_zero_capacity_fails_like_python_min() {
    let c = core().with_capacity(0);
    assert_eq!(c.enrol(&OWNER, None).unwrap_err().to_string(), "min() iterable argument is empty");
}

#[test]
fn test_set_last_who_clears_after_reset() {
    let core = Arc::new(core());
    let (fi, _, _) = driver(core.clone(), Some(OWNER.to_vec()), true);
    fi.process(&frame(), &[big()]).unwrap();
    assert_eq!(core.reset().unwrap(), 1);
    fi.set_last_who(None);
    let st = fi.status();
    assert!(st["prints"] == 0 && st["last_who"].is_null() && st["enrolled_at"].is_null());
}

// ---- CLI ---------------------------------------------------------------------

fn run_cli(action: &str, path: &Path, reply: Option<Value>) -> (Result<i32, eve_hooks::HookError>, String, Vec<Value>) {
    let mut posted = Vec::new();
    let mut out = Vec::new();
    let mut post = |event: Value| {
        posted.push(event);
        reply.clone()
    };
    let r = run_identity_with(action, path, || 0.8, &mut post, &mut out);
    (r, String::from_utf8(out).unwrap(), posted)
}

#[test]
fn test_cli_reset_through_the_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("o.json");
    let (r, out, posted) = run_cli("reset", &path, Some(json!({"ok": true, "removed": 3})));
    assert_eq!(r.unwrap(), 0);
    assert_eq!(posted, vec![json!({"evt": "identity", "action": "reset"})]);
    assert_eq!(out, format!("owner prints reset (3 removed) — {}\n", path.display()));
}

#[test]
fn test_cli_reset_without_daemon_removes_the_file() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("o.json");
    let c = core_at(&path);
    c.enrol(&OWNER, None).unwrap();
    c.enrol(&STRANGER, None).unwrap();
    c.save(None).unwrap();
    let (r, out, _) = run_cli("reset", &path, None);
    assert_eq!(r.unwrap(), 0);
    assert!(!path.exists());
    assert_eq!(
        out,
        format!(
            "daemon not reachable; removed {} (2 prints). A running daemon keeps its in-memory prints until restart.\n",
            path.display()
        )
    );
}

#[test]
fn test_cli_status_from_the_daemon() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("o.json");
    let reply = json!({"ok": true, "enabled": true, "identity": {
        "prints": 2, "capacity": 24, "threshold": 0.9, "path": "/p/o.json", "enrolled_at": null,
        "last_who": "owner", "last_distance": 0.125, "enrolments": 2}});
    let (r, out, posted) = run_cli("status", &path, Some(reply));
    assert_eq!(r.unwrap(), 0);
    assert_eq!(posted, vec![json!({"evt": "identity", "action": "status"})]);
    assert_eq!(
        out,
        "owner prints: 2/24  [daemon]\n\
         file:         /p/o.json\n\
         threshold:    0.9  (EVE_OWNER_THRESHOLD)\n\
         recognition:  on\n\
         last face:    owner (d=0.12)\n\
         enrolments:   2 this run\n"
    );
}

#[test]
fn test_cli_status_from_the_file_when_daemon_is_down() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("o.json");
    let (r, out, _) = run_cli("status", &path, None);
    assert_eq!(r.unwrap(), 0);
    assert_eq!(
        out,
        format!(
            "owner prints: 0/24  [file (daemon not reachable)]\n\
             file:         {}\n\
             threshold:    0.8  (EVE_OWNER_THRESHOLD)\n\
             \n\
             to enrol: hold Option while facing the robot for a few seconds\n",
            path.display()
        )
    );
    // A reply without an identity dict (an old daemon) also falls back to the file.
    let c = core_at(&path);
    c.enrol(&OWNER, Some(0.0)).unwrap();
    c.save(None).unwrap();
    let (_, out, _) = run_cli("status", &path, Some(json!({"ok": false})));
    assert!(out.starts_with("owner prints: 1/24  [file (daemon not reachable)]\n"), "{out}");
    assert!(out.contains("last enrol:   "), "{out}");
    assert!(!out.contains("to enrol"), "{out}");
}

#[test]
fn test_cli_reply_that_is_not_a_dict_fails_like_python() {
    let tmp = tempfile::tempdir().unwrap();
    let (r, _, _) = run_cli("status", &tmp.path().join("o.json"), Some(json!([1])));
    let e = r.unwrap_err();
    assert_eq!((e.kind.as_str(), e.message.as_str()), ("AttributeError", "'list' object has no attribute 'get'"));
}
