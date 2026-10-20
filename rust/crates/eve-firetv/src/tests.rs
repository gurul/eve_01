//! Port of `bridge/tests/test_firetv.py`: the connection (connect, get-state, reconnect once, mDNS), the state
//! read, power, keys, volume, mute, apps, YouTube search, typing, the screenshot, the code path, the tools,
//! construction and the CLI.
//!
//! The fake answers in the TV's own formats (read off a Fire TV on 2026-10-04), with documentation addresses
//! (192.0.2.x) and made-up serials.

use super::*;
use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;

const HOST: &str = "192.0.2.40";
const SERIAL: &str = "192.0.2.40:5555";
const NEW_HOST: &str = "192.0.2.41";
const MDNS: &str = "adb-TESTSERIAL0001";
const HOME_FOCUS: &str =
    "  mCurrentFocus=Window{fd3feb u0 com.amazon.tv.launcher/com.amazon.tv.launcher.ui.HomeActivity_vNext}";
const YT_FOCUS: &str = "  mCurrentFocus=Window{1a2b3c u0 com.amazon.firetv.youtube/dev.cobalt.app.MainActivity}";
const DREAM_FOCUS: &str =
    "  mCurrentFocus=Window{4c7b09a u0 com.amazon.ftv.screensaver/android.service.dreams.DreamActivity}";
const HDMI_FOCUS: &str = "  mCurrentFocus=Window{77aa u0 com.amazon.tv.inputpreference.service/\
                          com.amazon.tv.inputpreference.player.PassthroughPlayerActivity}";

fn png() -> Vec<u8> {
    let mut v = PNG_SIGNATURE.to_vec();
    v.extend_from_slice(b"\x00\x00\x00\rIHDR");
    v.extend_from_slice(&[0u8; 32]);
    v
}

fn no_sleep() -> Sleep {
    Arc::new(|_| Box::pin(async {}) as BoxFuture<'static, ()>)
}

/// The autouse fixture: the real probe is replaced by one that never answers.
fn silent_probe() -> Arc<dyn Probe> {
    Arc::new(|_h: String, _p: i64, _t: f64| async { false })
}

fn audio_block(volume: i64, muted: bool, max_index: i64) -> String {
    format!(
        "- STREAM_MUSIC:\n   Muted: {}\n   Muted Internally: false\n   Min: 0\n   Max: {max_index}\n   \
         streamVolume:{volume}\n   Current: 2 (speaker): {volume}, 4 (headset): 10, 400 (hdmi): 100, 40000000 \
         (default): 25\n   Devices: speaker\n",
        if muted { "true" } else { "false" }
    )
}

fn state_text(wake: &str, display: &str, focus: &str, volume: Option<i64>, muted: bool, max_index: i64) -> String {
    format!(
        "@@POWER\n  mWakefulness={wake}\nDisplay Power: state={display}\n@@FOCUS\n{focus}\n@@AUDIO\n{}@@END\n",
        volume.map_or(String::new(), |v| audio_block(v, muted, max_index))
    )
}

fn st_default() -> String {
    state_text("Awake", "ON", HOME_FOCUS, Some(44), false, 100)
}

fn st_focus(focus: &str) -> String {
    state_text("Awake", "ON", focus, Some(44), false, 100)
}

fn s(x: &str) -> String {
    x.to_string()
}

fn argv(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

/// The adb CLI and the TV behind it: records every argv, answers like the real thing.
struct Fake {
    calls: Vec<Vec<String>>,
    at: String,
    authorized: bool,
    wake: String,
    display: String,
    focus: String,
    volume: Option<i64>,
    max_index: i64,
    muted: bool,
    lose_next: u32,
    lost_answer: String,
    timeout_next: u32,
    mdns_rows: Vec<(String, String)>,
    broken: HashSet<String>,
    resolve: HashMap<String, String>,
    installed: Vec<String>,
    wake_works: bool,
    png: Vec<u8>,
    model: String,
    serialno: String,
    /// `fake.shell` replaced: the state read answers (0, "").
    blank_state: bool,
    /// `fake.shell` replaced: a volume `--set` is answered and ignored.
    ignore_set: bool,
    /// LossyMuteTv: (what the key answers, what the readback does).
    lossy: Option<(&'static str, &'static str)>,
}

struct FakeAdb(Mutex<Fake>);

impl FakeAdb {
    fn new() -> Arc<FakeAdb> {
        Arc::new(FakeAdb(Mutex::new(Fake {
            calls: Vec::new(),
            at: s(SERIAL),
            authorized: true,
            wake: s("Awake"),
            display: s("ON"),
            focus: s(HOME_FOCUS),
            volume: Some(44),
            max_index: 100,
            muted: false,
            lose_next: 0,
            lost_answer: String::new(),
            timeout_next: 0,
            mdns_rows: vec![(s(MDNS), s(SERIAL))],
            broken: HashSet::new(),
            resolve: HashMap::new(),
            installed: [
                "com.amazon.tv.launcher",
                "com.amazon.firetv.youtube",
                "com.netflix.ninja",
                "com.example.coolgame",
                "com.example.coolgame.helper",
                "com.example.weatherly",
            ]
            .iter()
            .map(|p| s(p))
            .collect(),
            wake_works: true,
            png: png(),
            model: s("AFTTEST1"),
            serialno: s("TESTSERIAL0001"),
            blank_state: false,
            ignore_set: false,
            lossy: None,
        })))
    }

    fn lossy(key: &'static str, readback: &'static str) -> Arc<FakeAdb> {
        let f = FakeAdb::new();
        f.0.lock().lossy = Some((key, readback));
        f
    }

    fn set(&self, f: impl FnOnce(&mut Fake)) {
        f(&mut self.0.lock());
    }

    fn get<T>(&self, f: impl FnOnce(&Fake) -> T) -> T {
        f(&self.0.lock())
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.0.lock().calls.clone()
    }

    fn answer(&self, argv: Vec<String>) -> (i32, String) {
        let mut f = self.0.lock();
        f.calls.push(argv.clone());
        let args = &argv[1..];
        if args.first().map(String::as_str) == Some("connect") {
            if args[1] != f.at {
                return (0, format!("failed to connect to '{}': Connection refused\n", args[1]));
            }
            return (0, format!("connected to {}\n", args[1]));
        }
        if args.first().map(String::as_str) == Some("disconnect") {
            return (0, format!("disconnected {}\n", args[1]));
        }
        if args.len() >= 2 && args[0] == "mdns" && args[1] == "services" {
            let rows: String = f
                .mdns_rows
                .iter()
                .map(|(n, hp)| {
                    let h = hp.split(':').next().unwrap_or("");
                    format!("{n}\t_adb._tcp\t{hp}\n{n}-Xy12Ab\t_adb-tls-connect._tcp\t{h}:41234\n")
                })
                .collect();
            return (0, format!("List of discovered mdns services\n{rows}"));
        }
        if args.first().map(String::as_str) == Some("-s") && args[2] == "get-state" {
            if args[1] != f.at {
                return (1, format!("error: device '{}' not found\n", args[1]));
            }
            return if f.authorized { (0, s("device\n")) } else { (1, s("error: device unauthorized.\n")) };
        }
        if args.first().map(String::as_str) == Some("-s") && args[2] == "shell" {
            if f.timeout_next > 0 {
                f.timeout_next -= 1;
                return (RC_TIMEOUT, String::new());
            }
            if f.lose_next > 0 {
                f.lose_next -= 1;
                // `adb shell` says "adb: ...", not "error: ..." like get-state (adb 1.0.41, 2026-10-04)
                if f.lost_answer.is_empty() {
                    return (1, format!("adb: device '{}' not found\n", args[1]));
                }
                return (1, f.lost_answer.clone());
            }
            let script = args[3].clone();
            return Self::shell(&mut f, &script);
        }
        (1, s("unknown\n"))
    }

    fn shell(f: &mut Fake, script: &str) -> (i32, String) {
        if let Some((key, readback)) = f.lossy {
            if script == format!("input keyevent {KEYCODE_VOLUME_MUTE}") {
                if readback == "timeout" {
                    f.timeout_next = 1; // the readback gets no answer...
                    f.at = s("10.0.0.99:5555"); // ...and the reconnect finds no TV
                    f.mdns_rows.clear();
                } else if readback == "no_muted" {
                    f.volume = None; // the readback answers without the audio block
                }
                return if key == "dropped" {
                    (0, String::new())
                } else {
                    (1, s("java.lang.SecurityException: injecting denied\n"))
                };
            }
        }
        if f.ignore_set && script.contains("--set") {
            return (0, String::new()); // the TV ignored it
        }
        if script == STATE_SCRIPT {
            if f.blank_state {
                return (0, String::new());
            }
            return (0, state_text(&f.wake, &f.display, &f.focus, f.volume, f.muted, f.max_index));
        }
        if script.contains("resolve-activity") {
            let pkg = script.split_whitespace().last().unwrap_or("");
            let comp = f.resolve.get(pkg).cloned().unwrap_or_default();
            let lean = if comp.is_empty() {
                s("No activity found\n")
            } else {
                format!("priority=0 preferredOrder=0 match=0x108000\n{comp}\n")
            };
            return (0, lean + "@@\nNo activity found\n");
        }
        if script.starts_with("getprop") {
            return (0, format!("{}\n@@\n{}\n", f.model, f.serialno));
        }
        let words = shell_words::split(script).unwrap_or_default();
        let w: Vec<&str> = words.iter().map(String::as_str).collect();
        if w.len() >= 2 && w[..2] == ["input", "keyevent"] {
            for c in &w[2..] {
                let c: i64 = c.parse().unwrap_or(-1);
                if c == KEYCODE_WAKEUP && f.wake_works {
                    f.wake = s("Awake");
                    f.display = s("ON");
                } else if c == KEYCODE_SLEEP {
                    f.wake = s("Asleep");
                    f.display = s("OFF");
                } else if c == KEYCODE_VOLUME_MUTE {
                    f.muted = !f.muted;
                }
            }
            return (0, String::new());
        }
        if w.len() >= 2 && w[..2] == ["input", "text"] {
            return (0, String::new());
        }
        if w.len() >= 3 && w[..3] == ["cmd", "media_session", "volume"] {
            let i = w.iter().position(|x| *x == "--set").expect("--set");
            let n: i64 = w[i + 1].parse().expect("an index");
            f.volume = Some(n);
            f.muted = n == 0;
            return (0, String::new());
        }
        if w.len() >= 3 && w[..3] == ["cmd", "media_session", "dispatch"] {
            return (0, String::new());
        }
        if w.len() >= 2 && w[..2] == ["am", "start"] {
            let i = w.iter().position(|x| *x == "-n").expect("-n");
            let comp = w[i + 1].to_string();
            if f.broken.contains(&comp) {
                return (
                    0,
                    format!(
                        "Starting: Intent {{ cmp={comp} }}\nError type 3\nError: Activity class {{{comp}}} does not exist.\n"
                    ),
                );
            }
            f.focus = format!("  mCurrentFocus=Window{{abc u0 {comp}}}");
            return (0, format!("Starting: Intent {{ cmp={comp} }}\n"));
        }
        if script == "pm list packages" {
            return (0, f.installed.iter().map(|p| format!("package:{p}\n")).collect());
        }
        (127, format!("/system/bin/sh: {}: not found\n", w.first().copied().unwrap_or("")))
    }

    fn shells(&self) -> Vec<String> {
        self.calls().into_iter().filter(|c| c.len() > 4 && c[3] == "shell").map(|c| c[4].clone()).collect()
    }

    fn sent(&self) -> Vec<String> {
        self.shells().into_iter().filter(|x| x != STATE_SCRIPT).collect()
    }
}

impl Runner for FakeAdb {
    fn run(&self, argv: Vec<String>, _timeout: f64) -> BoxFuture<'_, (i32, String)> {
        let out = self.answer(argv);
        Box::pin(async move { out })
    }
}

impl Capture for FakeAdb {
    fn capture(&self, argv: Vec<String>, _timeout: f64) -> BoxFuture<'_, (i32, Vec<u8>)> {
        let mut f = self.0.lock();
        f.calls.push(argv);
        let out = if f.lose_next > 0 {
            f.lose_next -= 1;
            (1, b"error: device offline\n".to_vec())
        } else {
            (0, f.png.clone())
        };
        Box::pin(async move { out })
    }
}

fn cfg(host: &str, mdns: Option<&str>) -> Config {
    let mut c = Config::new();
    c.insert("host".into(), json!(host));
    c.insert("port".into(), json!(5555));
    if let Some(m) = mdns {
        c.insert("mdns".into(), json!(m));
    }
    c
}

fn tv_with(fake: Arc<FakeAdb>, path: Option<PathBuf>) -> (FireTV, Arc<FakeAdb>) {
    let t = FireTV::new(cfg(HOST, Some(MDNS)))
        .with_runner(fake.clone())
        .with_capture(fake.clone())
        .with_adb("adb")
        .with_path(path)
        .with_sleep(no_sleep())
        .with_probe(silent_probe());
    (t, fake)
}

fn tv() -> (FireTV, Arc<FakeAdb>) {
    tv_with(FakeAdb::new(), None)
}

/// A FireTV over a closure runner (the tests' `async def runner`).
fn tv_runner<F>(c: Config, runner: F) -> FireTV
where
    F: Fn(Vec<String>) -> (i32, String) + Send + Sync + 'static,
{
    let runner = Arc::new(runner);
    FireTV::new(c)
        .with_runner(Arc::new(move |argv: Vec<String>, _t: f64| {
            let out = runner(argv);
            async move { out }
        }))
        .with_adb("adb")
        .with_sleep(no_sleep())
        .with_probe(silent_probe())
}

fn ca() -> ControlArgs {
    ControlArgs::default()
}

fn line(out: &Out) -> String {
    out.get("line").and_then(Value::as_str).unwrap_or("").to_string()
}

fn ok(out: &Out) -> bool {
    out.get("ok") == Some(&Value::Bool(true))
}

fn not_ok(out: &Out) -> bool {
    out.get("ok") == Some(&Value::Bool(false))
}

fn app_names() -> Value {
    Value::Array(APPS.iter().map(|a| json!(a.name)).collect())
}

// ---- the connection ----

#[tokio::test]
async fn test_connect_then_get_state_once_then_reuses_it() {
    let (t, fake) = tv();
    assert!(ok(&t.status().await));
    assert!(ok(&t.status().await));
    let verbs: Vec<String> =
        fake.calls().iter().map(|c| if c[1] != "-s" { c[1].clone() } else { c[3].clone() }).collect();
    assert_eq!(verbs.iter().filter(|v| *v == "connect").count(), 1);
    assert_eq!(verbs.iter().filter(|v| *v == "get-state").count(), 1);
}

#[tokio::test]
async fn test_already_connected_counts_as_connected() {
    let fake = FakeAdb::new();
    let f2 = fake.clone();
    let t = tv_runner(cfg(HOST, Some(MDNS)), move |argv| {
        if argv[1] == "connect" {
            return (0, format!("already connected to {SERIAL}\n"));
        }
        f2.answer(argv)
    });
    assert!(t.ensure().await.ok);
}

#[tokio::test]
async fn test_connect_failure_words_with_exit_code_zero() {
    for reply in [
        "failed to connect to '192.0.2.40:5555': Connection refused",
        "unable to connect to 192.0.2.40:5555: No route to host",
        "cannot connect to 192.0.2.40:5555: Operation timed out",
        "",
    ] {
        let t = tv_runner(cfg(HOST, Some(MDNS)), move |argv| {
            if argv[1] == "mdns" {
                return (0, s("List of discovered mdns services\n"));
            }
            (0, s(reply))
        });
        let r = t.ensure().await;
        assert!(!r.ok && r.why == "unreachable" && r.line == UNREACHABLE_LINE, "{reply}");
    }
}

#[tokio::test]
async fn test_unauthorized_is_its_own_outcome_and_says_the_allow_prompt() {
    let (t, fake) = tv();
    fake.set(|f| f.authorized = false);
    let out = t.status().await;
    assert!(not_ok(&out) && out["authorized"] == json!(false) && out["reachable"] == json!(true));
    assert!(line(&out).contains("Allow USB debugging") && line(&out).contains("Always allow"));
    assert!(!fake.calls().iter().any(|c| c[1] == "mdns")); // nothing to rediscover: the TV answered
}

#[tokio::test]
async fn test_offline_device_is_unreachable_after_one_rediscovery() {
    let t = tv_runner(cfg(HOST, Some(MDNS)), |argv| {
        if argv[1] == "connect" {
            return (0, format!("connected to {SERIAL}"));
        }
        if argv[1] == "mdns" {
            return (0, s("List of discovered mdns services\n"));
        }
        if argv[1] == "-s" && argv[3] == "get-state" {
            return (1, s("error: device offline\n"));
        }
        (0, String::new())
    });
    let r = t.ensure().await;
    assert!(!r.ok && r.why == "offline");
}

#[tokio::test]
async fn test_lost_mid_command_disconnects_reconnects_and_retries_once() {
    let (t, fake) = tv();
    t.ensure().await;
    fake.set(|f| f.lose_next = 1);
    assert!(ok(&t.status().await));
    let verbs: Vec<String> = fake.calls().iter().map(|c| c[1].clone()).collect();
    assert!(verbs.contains(&s("disconnect")) && verbs.iter().filter(|v| *v == "connect").count() == 2);
    assert!(!fake.calls().iter().any(|c| c.contains(&s("kill-server"))));
}

#[tokio::test]
async fn test_lost_twice_fails_fast_without_a_third_try() {
    let (t, fake) = tv();
    t.ensure().await;
    fake.set(|f| f.lose_next = 5);
    let out = t.status().await;
    assert!(not_ok(&out));
    assert_eq!(fake.shells().iter().filter(|x| *x == STATE_SCRIPT).count(), 2);
}

#[tokio::test]
async fn test_a_timeout_is_an_outcome_and_reconnects() {
    let (t, fake) = tv();
    t.ensure().await;
    fake.set(|f| f.timeout_next = 1);
    assert!(ok(&t.status().await));
}

#[tokio::test]
async fn test_adb_shells_own_offline_words_reconnect_too() {
    let (t, fake) = tv();
    t.ensure().await;
    fake.set(|f| {
        f.lose_next = 1;
        f.lost_answer = s("adb: device offline\n");
    });
    assert!(ok(&t.status().await));
    assert_eq!(fake.calls().iter().filter(|c| c[1] == "disconnect").count(), 1);
}

#[tokio::test]
async fn test_a_lost_tv_is_never_stuck_ready() {
    // the adb server restarted under eve: every shell says "not found" until eve connects again
    let (t, fake) = tv();
    t.ensure().await;
    fake.set(|f| f.lose_next = 1);
    assert_eq!(line(&t.control("power_on", ca()).await), "The TV is already on.");
    assert!(!lost(1, "adb: error: something on the TV failed"));
}

#[tokio::test]
async fn test_a_timeout_never_types_or_presses_twice() {
    let (t, fake) = tv();
    t.ensure().await;
    let count = |prefix: &str| fake.shells().iter().filter(|x| x.starts_with(prefix)).count();
    fake.set(|f| f.timeout_next = 1);
    let out = t.type_text("lofi").await;
    assert!(not_ok(&out) && line(&out) == TIMEOUT_LINE);
    assert_eq!(count("input text"), 1);
    fake.set(|f| f.timeout_next = 1);
    assert_eq!(line(&t.press("down", 3).await), TIMEOUT_LINE);
    assert_eq!(count("input keyevent"), 1);
    fake.set(|f| f.timeout_next = 1);
    assert_eq!(line(&t.control("play_pause", ca()).await), TIMEOUT_LINE);
    // the next command connects afresh and works
    assert!(ok(&t.press("down", 1).await));
    // "not found" before the command ran: it never landed, so it is sent again
    fake.set(|f| f.lose_next = 1);
    assert!(ok(&t.type_text("lofi").await));
    assert_eq!(count("input text"), 3);
}

#[tokio::test]
async fn test_a_state_read_without_the_state_connects_afresh_next_time() {
    let (t, fake) = tv();
    t.ensure().await;
    fake.set(|f| f.blank_state = true);
    let (_, st) = t.state().await;
    assert!(st.is_none() && !t.ready.load(Ordering::SeqCst));
    fake.set(|f| f.blank_state = false);
    assert!(ok(&t.status().await));
}

#[tokio::test]
async fn test_a_cancelled_adb_call_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let pid_file = dir.path().join("pid");
    let script = format!("echo $$ > {}; exec sleep 30", pid_file.display());
    let task = tokio::spawn(adb_runner(argv(&["/bin/sh", "-c", &script]), 60.0));
    for _ in 0..100 {
        tokio::time::sleep(Duration::from_millis(20)).await;
        if std::fs::read_to_string(&pid_file).is_ok_and(|t| !t.trim().is_empty()) {
            break;
        }
    }
    task.abort();
    let err = task.await.expect_err("the call was cancelled");
    assert!(err.is_cancelled());
    let pid: i32 = std::fs::read_to_string(&pid_file).unwrap().trim().parse().unwrap();
    // SAFETY: signal 0 only checks that the process exists.
    let alive = unsafe { libc::kill(pid, 0) } == 0;
    assert!(!alive, "the adb child {pid} outlived its cancelled call");
    assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
}

#[tokio::test]
async fn test_mdns_rediscovery_moves_to_the_new_address_and_saves_it() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("firetv.json");
    save(&cfg(HOST, Some(MDNS)), Some(&p)).unwrap();
    let fake = FakeAdb::new();
    fake.set(|f| {
        f.at = format!("{NEW_HOST}:5555"); // DHCP gave the TV a new address
        f.mdns_rows = vec![(s("adb-SOMEONEELSE9"), s("192.0.2.77:5555")), (s(MDNS), format!("{NEW_HOST}:5555"))];
    });
    let (t, _) = tv_with(fake, Some(p.clone()));
    let out = t.status().await;
    assert!(ok(&out) && t.serial() == format!("{NEW_HOST}:5555"));
    assert_eq!(load(Some(&p))["host"], json!(NEW_HOST));
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
}

#[test]
fn test_parse_mdns_keeps_plain_adb_only() {
    let text = format!(
        "List of discovered mdns services\n{MDNS}\t_adb._tcp\t{SERIAL}\n{MDNS}-Ab12Cd\t_adb-tls-connect._tcp\t{HOST}:41234\n"
    );
    assert_eq!(parse_mdns(&text), vec![(s(MDNS), s(HOST), 5555)]);
}

#[tokio::test]
async fn test_no_adb_says_how_to_install() {
    let t = FireTV::new(cfg(HOST, None)).with_runner(FakeAdb::new()).with_adb("").with_sleep(no_sleep());
    let out = t.control("home", ca()).await;
    assert!(not_ok(&out) && line(&out).contains("brew install --cask android-platform-tools"));
}

#[test]
fn test_find_adb_override_first() {
    let env: Environ = [(s("EVE_ADB"), s("/opt/x/adb"))].into_iter().collect();
    assert_eq!(find_adb(Some(&env)), "/opt/x/adb");
}

#[tokio::test]
async fn test_real_runner_times_out_as_an_outcome() {
    let (rc, out) = adb_runner(argv(&["/bin/sleep", "5"]), 0.2).await;
    assert!(rc == RC_TIMEOUT && out.is_empty());
    let (rc, out) = adb_runner(argv(&["/nonexistent/adb-binary"]), 1.0).await;
    assert!(rc == RC_NO_ADB && out.contains("could not start"), "{out}");
}

// ---- the state ----

#[test]
fn test_parse_state_awake_home_volume() {
    let st = parse_state(&st_default());
    assert_eq!((st.power.as_str(), st.app.as_str(), st.volume(), st.muted), ("on", "home", Some(44), Some(false)));
    assert_eq!(said_state(&st), "TV on — home, volume 44%");
}

#[test]
fn test_parse_state_screensaver_is_on() {
    let st = parse_state(&state_text("Dreaming", "ON", DREAM_FOCUS, Some(44), false, 100));
    assert!(st.power == "screensaver" && st.app == "screensaver");
}

#[test]
fn test_parse_state_standby() {
    for (wake, display) in [("Asleep", "OFF"), ("Dozing", "DOZE"), ("Awake", "OFF")] {
        let st = parse_state(&state_text(wake, display, HOME_FOCUS, Some(44), false, 100));
        assert!(st.power == "standby" && said_state(&st) == "TV in standby, volume 44%.");
        // with no volume to read it says only the power
        let st = parse_state(&state_text(wake, display, HOME_FOCUS, None, false, 100));
        assert_eq!(said_state(&st), "TV in standby.");
    }
}

#[test]
fn test_parse_state_foreground_names() {
    assert_eq!(parse_state(&st_focus(YT_FOCUS)).app, "YouTube");
    assert_eq!(parse_state(&st_focus(HDMI_FOCUS)).app, "an HDMI input");
    let resumed = "    mResumedActivity: ActivityRecord{1fceaad u0 com.netflix.ninja/.MainActivity t18}";
    let st = parse_state(&st_focus(resumed));
    assert!(st.app == "Netflix" && st.activity == "com.netflix.ninja.MainActivity");
    assert_eq!(parse_state(&st_focus("  mCurrentFocus=null")).app, "");
}

#[test]
fn test_parse_state_projectivy_is_home() {
    let focus = "  mCurrentFocus=Window{9d1e u0 com.spocky.projengmenu/com.spocky.projengmenu.ui.home.MainActivity}";
    assert_eq!(parse_state(&st_focus(focus)).app, "home");
    let settings = "  mCurrentFocus=Window{9d1f u0 com.spocky.projengmenu/\
                    com.spocky.projengmenu.ui.guidedActions.activities.settings.AppSettingsActivity}";
    assert_eq!(parse_state(&st_focus(settings)).app, "Projectivy settings");
}

#[test]
fn test_parse_state_missing_volume_is_unknown_not_guessed() {
    let st = parse_state(&state_text("Awake", "ON", HOME_FOCUS, None, false, 100));
    assert!(st.volume().is_none() && st.muted.is_none() && said_state(&st).contains("volume unknown"));
}

#[test]
fn test_parse_state_scales_to_the_tvs_max() {
    assert_eq!(parse_state(&state_text("Awake", "ON", HOME_FOCUS, Some(6), false, 15)).volume(), Some(40));
}

#[tokio::test]
async fn test_state_script_is_one_round_trip_and_never_grep_m1() {
    assert!(!STATE_SCRIPT.contains("-m1") && !STATE_SCRIPT.contains("-m 1"));
    for part in ["mWakefulness=", "Display Power: state=", "mCurrentFocus", "mResumedActivity", "STREAM_MUSIC"] {
        assert!(STATE_SCRIPT.contains(part));
    }
    let (t, fake) = tv();
    t.status().await;
    assert_eq!(fake.shells().len(), 1);
}

#[test]
fn test_sanitise_strips_controls_and_caps() {
    assert_eq!(clean("Evil\nApp\x1b[31m\u{202e}Name\x00", CLEAN_CAP), "Evil App [31m Name");
    assert_eq!(clean(&"x".repeat(500), CLEAN_CAP).chars().count(), CLEAN_CAP);
    let weird = "  mCurrentFocus=Window{1 u0 com.evil.app\x1b/.Main}";
    assert!(!parse_state(&st_focus(weird)).app.contains('\x1b'));
}

// ---- power ----

#[tokio::test]
async fn test_power_on_sends_wakeup_and_polls_until_awake() {
    let (t, fake) = tv();
    fake.set(|f| {
        f.wake = s("Asleep");
        f.display = s("OFF");
    });
    let out = t.control("power_on", ca()).await;
    assert!(ok(&out) && line(&out) == "TV on — home." && out["power"] == json!("on"));
    assert_eq!(fake.sent(), vec!["input keyevent 224"]);
}

#[tokio::test]
async fn test_power_on_when_already_on_sends_nothing() {
    let (t, fake) = tv();
    assert_eq!(line(&t.control("power_on", ca()).await), "The TV is already on.");
    assert!(fake.sent().is_empty());
}

#[tokio::test]
async fn test_power_on_that_does_not_take_reports_what_the_tv_says() {
    let (t, fake) = tv();
    fake.set(|f| {
        f.wake = s("Asleep");
        f.display = s("OFF");
        f.wake_works = false;
    });
    let out = t.control("power_on", ca()).await;
    assert!(not_ok(&out) && line(&out).contains("still reports standby"));
}

#[tokio::test]
async fn test_power_on_unreachable_says_deep_standby() {
    let t = tv_runner(cfg(HOST, Some(MDNS)), |argv| {
        if argv[1] == "mdns" {
            (0, s("List of discovered mdns services\n"))
        } else {
            (0, s("failed to connect to x"))
        }
    });
    let out = t.control("power_on", ca()).await;
    assert!(not_ok(&out) && line(&out).contains("deep standby") && line(&out).contains("remote"));
}

#[tokio::test]
async fn test_power_off_sends_sleep() {
    let (t, fake) = tv();
    let out = t.control("power_off", ca()).await;
    assert!(ok(&out) && line(&out) == "TV off.");
    assert_eq!(fake.sent(), vec!["input keyevent 223"]);
}

#[tokio::test]
async fn test_screensaver_counts_as_on_for_power_off() {
    let (t, fake) = tv();
    fake.set(|f| {
        f.wake = s("Dreaming");
        f.focus = s(DREAM_FOCUS);
    });
    assert!(ok(&t.control("power_off", ca()).await) && fake.sent() == vec!["input keyevent 223"]);
}

// ---- keys ----

#[tokio::test]
async fn test_navigation_is_batched_in_one_call_in_order() {
    let (t, fake) = tv();
    let out = t.control("down", ControlArgs { repeat: Some(3), ..ca() }).await;
    assert!(ok(&out) && line(&out) == "Pressed down 3 times.");
    assert_eq!(fake.sent(), vec!["input keyevent 20 20 20"]);
    t.control("select", ca()).await;
    assert_eq!(fake.sent().last().unwrap(), "input keyevent 23");
}

#[tokio::test]
async fn test_navigation_repeat_is_capped() {
    let (t, fake) = tv();
    let out = t.control("right", ControlArgs { repeat: Some(99), ..ca() }).await;
    assert_eq!(out["pressed"], json!(MAX_REPEAT));
    assert_eq!(fake.sent(), vec![format!("input keyevent {}", vec!["22"; MAX_REPEAT as usize].join(" "))]);
    t.control("left", ControlArgs { repeat: Some(0), ..ca() }).await;
    assert_eq!(fake.sent().last().unwrap(), "input keyevent 21");
}

#[tokio::test]
async fn test_media_keys_go_through_media_session_never_input() {
    for (action, key) in [
        ("play_pause", "play-pause"),
        ("play", "play"),
        ("pause", "pause"),
        ("next", "next"),
        ("previous", "previous"),
        ("rewind", "rewind"),
        ("fast_forward", "fast-forward"),
    ] {
        let (t, fake) = tv();
        assert!(ok(&t.control(action, ca()).await));
        assert_eq!(fake.sent(), vec![format!("cmd media_session dispatch {key}")]);
    }
}

// ---- volume and mute ----

#[tokio::test]
async fn test_volume_absolute_maps_percent_to_index_and_reads_back() {
    let (t, fake) = tv();
    fake.set(|f| {
        f.max_index = 15;
        f.volume = Some(6);
    });
    let out = t.control("volume", ControlArgs { volume: Some(60), ..ca() }).await;
    assert_eq!(fake.sent(), vec!["cmd media_session volume --stream 3 --set 9"]);
    assert!(ok(&out) && out["volume"] == json!(60) && line(&out) == "TV volume 60%.");
}

#[tokio::test]
async fn test_volume_relative_reads_then_sets_and_clamps() {
    let (t, fake) = tv();
    let change = |c| ControlArgs { volume_change: Some(c), ..ca() };
    assert_eq!(t.control("volume", change(10)).await["volume"], json!(54));
    fake.set(|f| f.volume = Some(97));
    assert_eq!(t.control("volume", change(10)).await["volume"], json!(100));
    fake.set(|f| f.volume = Some(3));
    assert_eq!(t.control("volume", change(-10)).await["volume"], json!(0));
    assert_eq!(t.control("volume", ControlArgs { volume: Some(250), ..ca() }).await["volume"], json!(100));
}

#[tokio::test]
async fn test_volume_reports_the_read_value_not_the_asked_one() {
    let (t, fake) = tv();
    fake.set(|f| f.ignore_set = true);
    let out = t.control("volume", ControlArgs { volume: Some(30), ..ca() }).await;
    assert!(out["volume"] == json!(44) && line(&out) == "TV volume 44%.");
}

#[tokio::test]
async fn test_volume_unreadable_refuses_to_guess() {
    let (t, fake) = tv();
    fake.set(|f| f.volume = None);
    let out = t.control("volume", ControlArgs { volume_change: Some(10), ..ca() }).await;
    assert!(not_ok(&out) && line(&out).contains("couldn't read"));
    assert!(fake.sent().is_empty());
}

#[tokio::test]
async fn test_mute_toggles_only_when_needed() {
    let (t, fake) = tv();
    let out = t.control("mute", ca()).await;
    assert!(ok(&out) && out["muted"] == json!(true) && fake.sent() == vec!["input keyevent 164"]);
    assert_eq!(line(&t.control("mute", ca()).await), "The TV is already muted.");
    assert_eq!(fake.sent(), vec!["input keyevent 164"]);
    let out = t.control("unmute", ca()).await;
    assert!(ok(&out) && out["muted"] == json!(false) && line(&out) == "TV unmuted, volume 44%.");
    assert_eq!(fake.sent(), vec!["input keyevent 164", "input keyevent 164"]);
    assert!(!fake.sent().iter().any(|x| x.contains("dispatch mute"))); // that one is the microphone's key
}

#[tokio::test]
async fn test_mute_lost_and_unread_is_not_ok() {
    // FireTv.lean unreadTrace / code_violates_unread_ok: unmuted, "mute", the key is answered but lost, the
    // readback times out and the reconnect fails. Before the fix this said ok while the TV stayed unmuted.
    let (t, fake) = tv_with(FakeAdb::lossy("dropped", "timeout"), None);
    let out = t.control("mute", ca()).await;
    assert!(!fake.get(|f| f.muted) && fake.sent() == vec!["input keyevent 164"]);
    assert!(not_ok(&out) && line(&out).contains("couldn't"));
}

#[tokio::test]
async fn test_mute_refused_key_is_not_ok() {
    // FireTv.lean code_violates_refused_key: the key is refused (rc != 0) and the readback has no Muted: line.
    let (t, fake) = tv_with(FakeAdb::lossy("refused", "no_muted"), None);
    let out = t.control("mute", ca()).await;
    assert!(!fake.get(|f| f.muted) && fake.sent() == vec!["input keyevent 164"]);
    assert!(not_ok(&out) && line(&out).contains("refused"));
}

#[tokio::test]
async fn test_mute_dropped_key_with_a_readback_says_so() {
    // positive control for the readback: the key is answered but lost, the TV reads back unmuted: not ok
    let (t, _) = tv_with(FakeAdb::lossy("dropped", "read"), None);
    let out = t.control("mute", ca()).await;
    assert!(not_ok(&out) && line(&out).contains("still reports unmuted"));
}

// ---- apps, search, typing ----

fn app_args(app: &str) -> ControlArgs {
    ControlArgs { app: s(app), ..ca() }
}

#[tokio::test]
async fn test_open_app_by_alias_pins_the_component() {
    let (t, fake) = tv();
    let out = t.control("open_app", app_args("Amazon Prime")).await;
    assert!(ok(&out) && line(&out) == "Opened Prime Video.");
    assert_eq!(fake.sent(), vec!["am start -n com.amazon.firebat/com.amazon.firebatcore.deeplink.DeepLinkRoutingActivity"]);
}

#[tokio::test]
async fn test_open_app_says_what_it_interrupted() {
    let (t, fake) = tv();
    fake.set(|f| f.focus = s(YT_FOCUS));
    let out = t.control("open_app", app_args("netflix")).await;
    assert!(out["interrupted"] == json!("YouTube") && line(&out).contains("YouTube was on"));
}

#[tokio::test]
async fn test_open_app_wakes_a_sleeping_tv_first() {
    let (t, fake) = tv();
    fake.set(|f| {
        f.wake = s("Asleep");
        f.display = s("OFF");
    });
    let out = t.control("open_app", app_args("Disney+")).await;
    assert!(line(&out).starts_with("Turned the TV on. Opened Disney+"));
    assert_eq!(fake.sent()[0], "input keyevent 224");
}

#[tokio::test]
async fn test_open_app_reresolves_when_the_activity_moved() {
    let (t, fake) = tv();
    let old = find_app("netflix").unwrap().component;
    fake.set(|f| {
        f.broken.insert(s(old));
        f.resolve.insert(s("com.netflix.ninja"), s("com.netflix.ninja/.NewMainActivity"));
    });
    let out = t.control("open_app", app_args("Netflix")).await;
    assert!(ok(&out) && fake.sent().last().unwrap() == "am start -n com.netflix.ninja/.NewMainActivity");
    assert!(fake.sent().iter().any(|x| x.contains("LEANBACK_LAUNCHER com.netflix.ninja")));
}

#[tokio::test]
async fn test_open_unknown_app_by_installed_package() {
    let (t, fake) = tv();
    fake.set(|f| {
        f.resolve.insert(s("com.example.weatherly"), s("com.example.weatherly/.TvActivity"));
    });
    let out = t.control("open_app", app_args("Weatherly")).await;
    assert!(ok(&out) && fake.sent().last().unwrap() == "am start -n com.example.weatherly/.TvActivity");
}

#[tokio::test]
async fn test_open_unknown_app_not_installed_or_ambiguous() {
    let (t, fake) = tv();
    assert!(line(&t.control("open_app", app_args("Tubi")).await).contains("no app called"));
    assert!(line(&t.control("open_app", app_args("coolgame")).await).contains("More than one"));
    assert!(!fake.sent().iter().any(|x| x.starts_with("am start")));
}

#[tokio::test]
async fn test_youtube_search_encodes_and_pins_youtube() {
    let (t, fake) = tv();
    let out = t.control("youtube_search", ControlArgs { query: s("lofi beats & rain 'live'"), ..ca() }).await;
    assert!(ok(&out));
    let words = shell_words::split(fake.sent().last().unwrap()).unwrap();
    assert_eq!(words[..2], ["am", "start"]);
    let n = words.iter().position(|w| w == "-n").unwrap();
    assert_eq!(words[n + 1], YOUTUBE.component);
    let d = words.iter().position(|w| w == "-d").unwrap();
    assert_eq!(words[d + 1], "https://www.youtube.com/results?search_query=lofi+beats+%26+rain+%27live%27");
}

#[tokio::test]
async fn test_type_text_escapes_spaces_and_quotes_for_the_remote_shell() {
    let (t, fake) = tv();
    let out = t.control("type_text", ControlArgs { text: s("it's a \"test\"; rm -rf $HOME"), ..ca() }).await;
    assert!(ok(&out) && line(&out) == "Typed 27 characters on the TV.");
    let last = fake.sent().last().unwrap().clone();
    let words = shell_words::split(&last).unwrap();
    assert_eq!(words, vec!["input", "text", "it's%sa%s\"test\";%srm%s-rf%s$HOME"]);
    assert!(last.starts_with("input text '")); // one quoted word: the TV's shell runs nothing
}

#[tokio::test]
async fn test_type_text_refuses() {
    for (text, why) in
        [("café".to_string(), "plain English"), ("x".repeat(201), "too long"), (s(""), "Nothing"), (s("line\nbreak"), "plain English")]
    {
        let (t, fake) = tv();
        let out = t.control("type_text", ControlArgs { text: text.clone(), ..ca() }).await;
        assert!(not_ok(&out) && line(&out).contains(why) && fake.calls().is_empty(), "{text:?}");
    }
}

// ---- the screenshot ----

#[tokio::test]
async fn test_screenshot_saves_a_png() {
    let dir = tempfile::tempdir().unwrap();
    let (t, fake) = tv();
    let out = t.screenshot(Some(dir.path()), None).await.unwrap();
    let path = out["path"].as_str().unwrap();
    assert!(ok(&out) && std::fs::read(path).unwrap().starts_with(PNG_SIGNATURE));
    assert!(fake.calls().contains(&argv(&["adb", "-s", SERIAL, "exec-out", "screencap", "-p"])));
}

#[tokio::test]
async fn test_screenshot_rejects_what_is_not_a_png() {
    let dir = tempfile::tempdir().unwrap();
    let (t, fake) = tv();
    fake.set(|f| f.png = b"error: something\n".to_vec());
    let out = t.screenshot(Some(dir.path()), None).await.unwrap();
    assert!(not_ok(&out) && std::fs::read_dir(dir.path()).unwrap().next().is_none());
}

// ---- the code path ----

fn match_cases() -> Vec<(&'static str, Command)> {
    let c = Command::new;
    vec![
        ("turn on the tv", c("power_on")),
        ("Turn the TV off", c("power_off")),
        ("tv on", c("power_on")),
        ("tv off", c("power_off")),
        ("hey eve, turn on the fire tv please", c("power_on")),
        ("switch off the television", c("power_off")),
        ("tv volume up", c("volume").with_change(10)),
        ("turn the tv up", c("volume").with_change(10)),
        ("turn the tv down a bit", c("volume").with_change(-5)),
        ("turn the tv down", c("volume").with_change(-10)),
        ("tv volume 30", c("volume").with_volume(30)),
        ("set the tv volume to 30", c("volume").with_volume(30)),
        ("set the volume on the tv to 25%", c("volume").with_volume(25)),
        ("mute the tv", c("mute")),
        ("unmute the tv", c("unmute")),
        ("pause the tv", c("pause")),
        ("play the tv", c("play")),
        ("resume the tv", c("play")),
        ("tv home", c("home")),
        ("go home on the tv", c("home")),
        ("tv down 3 times", c("down").with_repeat(3)),
        ("tv up twice", c("up").with_repeat(2)),
        ("tv left", c("left")),
        ("press up on the tv", c("up")),
        ("what's on fire tv", c("status")),
        ("press ok on the tv", c("select")),
        ("open youtube on the tv", c("open_app").with_app("youtube")),
        ("put netflix on the tv", c("open_app").with_app("netflix")),
        ("put disney+ on the telly", c("open_app").with_app("disney plus")),
        ("search lofi beats on youtube on the tv", c("youtube_search").with_query("lofi beats")),
        ("what's on the tv", c("status")),
        ("is the tv on?", c("status")),
        ("tv status", c("status")),
    ]
}

#[test]
fn test_match() {
    let (t, _) = tv();
    for (text, cmd) in match_cases() {
        assert_eq!(match_command(text), Some(cmd.clone()), "{text}");
        assert_eq!(t.match_command(text), Some(cmd), "{text}");
    }
}

#[test]
fn test_match_leaves_the_rest_to_the_model() {
    for text in [
        "pause",
        "stop",
        "pause the music",
        "lights off",
        "play music on the tv",
        "put the music on the tv",
        "play spotify on the tv",
        "turn on the lights",
        "what is a tv",
        "open youtube",
        "play daft punk on spotify",
        "play daft punk on the tv",
        "turn on the tv in ten minutes",
        "tv volume 400",
        "volume up",
        "mute",
        "why is the tv so loud",
        "tv up",
        "tv down",
        "what's on tv",
        "what is on tv tonight",
    ] {
        assert_eq!(match_command(text), None, "{text}");
    }
}

#[tokio::test]
async fn test_run_by_code_says_what_happened() {
    let (t, _) = tv();
    assert_eq!(t.run(&Command::new("status")).await, "TV on — home, volume 44%");
    assert_eq!(t.run(&Command::new("volume").with_volume(20)).await, "TV volume 20%.");
    assert_eq!(t.run(&Command::new("down").with_repeat(2)).await, "Pressed down 2 times.");
}

// ---- the tools ----

#[test]
fn test_tools_are_strict_and_complete() {
    for t in TOOLS.iter() {
        let params = &t["parameters"];
        assert!(t["strict"] == json!(true) && params["additionalProperties"] == json!(false));
        let mut required: Vec<String> =
            params["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
        let mut props: Vec<String> = params["properties"].as_object().unwrap().keys().cloned().collect();
        required.sort();
        props.sort();
        assert_eq!(required, props);
    }
    let names: Vec<&str> = TOOLS.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(names, TOOL_NAMES);
    assert_eq!(TOOL_NAMES, ["firetv_status", "firetv_control", "firetv_screenshot"]);
    let actions: Vec<&str> = TOOLS[1]["parameters"]["properties"]["action"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(!actions.contains(&"stop") && ["power_on", "type_text", "youtube_search"].iter().all(|a| actions.contains(a)));
    assert_eq!(*TOOLS, tv().0.tools());
}

#[tokio::test]
async fn test_handle_routes() {
    let dir = tempfile::tempdir().unwrap();
    let (t, _) = tv();
    let t = t.with_shots_dir(Some(dir.path().to_path_buf()));
    assert_eq!(t.handle("firetv_status", &json!({})).await.unwrap()["volume"], json!(44));
    let args = json!({"action": "volume", "app": null, "query": null, "text": null, "volume": null,
                      "volume_change": -10, "repeat": null});
    assert_eq!(t.handle("firetv_control", &args).await.unwrap()["volume"], json!(34));
    let mut args2 = args.clone();
    args2["action"] = json!("up");
    args2["volume_change"] = Value::Null;
    args2["repeat"] = json!(2);
    assert_eq!(line(&t.handle("firetv_control", &args2).await.unwrap()), "Pressed up 2 times.");
    let shot = t.handle("firetv_screenshot", &json!({})).await.unwrap();
    let keys: HashSet<&str> = shot.keys().map(String::as_str).collect();
    assert!(keys == HashSet::from(["ok", "path", "shows"]) && ok(&shot));
    assert_eq!(shot["shows"], json!("TV on — home, volume 34%")); // the model can't see the picture: it gets this
    let mut boom = args2.clone();
    boom["action"] = json!("explode");
    assert!(not_ok(&t.handle("firetv_control", &boom).await.unwrap()));
    assert!(not_ok(&t.handle("nope", &json!({})).await.unwrap()));
}

#[test]
fn test_instructions_name_the_limits() {
    let (t, _) = tv();
    let text = t.instructions(true);
    assert!(text.contains("HDMI") && text.contains("interrupted") && text.contains("firetv_screenshot") && text.contains("don't see it"));
    let voice = t.instructions(false);
    assert!(voice.contains("HDMI") && !voice.contains("screenshot"));
}

// ---- construction and the CLI ----

fn env(pairs: &[(&str, &str)]) -> Environ {
    pairs.iter().map(|(k, v)| (s(k), s(v))).collect()
}

#[test]
fn test_make_firetv_off_switch_and_not_paired() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("firetv.json");
    assert!(make_firetv(Some(&p), Some(&env(&[])), None, None).is_none());
    save(&cfg(HOST, Some(MDNS)), Some(&p)).unwrap();
    for off in ["0", "false", "no", "OFF"] {
        assert!(make_firetv(Some(&p), Some(&env(&[("EVE_FIRETV", off)])), None, None).is_none());
    }
    let hub = make_firetv(Some(&p), Some(&env(&[("EVE_ADB", "/x/adb")])), Some(FakeAdb::new()), None);
    assert_eq!(hub.expect("a hub").adb, "/x/adb");
    assert!(enabled(Some(&env(&[]))) && !enabled(Some(&env(&[("EVE_FIRETV", "0")]))));
}

#[test]
fn test_config_is_written_private() {
    let dir = tempfile::tempdir().unwrap();
    let p = save(&cfg(HOST, None), Some(&dir.path().join("sub").join("firetv.json"))).unwrap();
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(Value::Object(load(Some(&p))), json!({"host": HOST, "port": 5555, "mdns": ""}));
    std::fs::write(dir.path().join("bad.json"), "{}").unwrap();
    assert!(load(Some(&dir.path().join("bad.json"))).is_empty());
}

/// `F.cli(argv, path=p, out=lines.append, runner=fake, environ=env)`.
fn run_cli(args: &[&str], p: &Path, fake: Option<Arc<FakeAdb>>, environ: Option<Environ>, lines: &mut Vec<String>) -> i32 {
    let mut out = |l: &str| lines.push(l.to_string());
    let opts = CliOptions {
        path: Some(p.to_path_buf()),
        runner: fake.clone().map(|f| f as Arc<dyn Runner>),
        capture: fake.map(|f| f as Arc<dyn Capture>),
        environ,
    };
    cli(&argv(args), &mut out, opts)
}

#[test]
fn test_cli_pair_by_mdns_then_status() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("firetv.json");
    let fake = FakeAdb::new();
    let mut lines = Vec::new();
    let e = || Some(env(&[("EVE_ADB", "adb")]));
    assert_eq!(run_cli(&["status"], &p, Some(fake.clone()), e(), &mut lines), 1);
    assert!(lines[0].contains("firetv pair"));
    lines.clear();
    assert_eq!(run_cli(&["pair"], &p, Some(fake.clone()), e(), &mut lines), 0);
    let saved = load(Some(&p));
    assert!(saved["host"] == json!(HOST) && saved["mdns"] == json!(MDNS) && saved["model"] == json!("AFTTEST1"));
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o600);
    lines.clear();
    assert_eq!(run_cli(&["status"], &p, Some(fake.clone()), e(), &mut lines), 0);
    assert_eq!(lines, vec![s("TV on — home, volume 44%"), format!("AFTTEST1 at {SERIAL}")]);
    assert_eq!(run_cli(&["forget"], &p, None, None, &mut lines), 0);
    assert!(!p.exists());
}

#[test]
fn test_cli_pair_unauthorized_and_by_host() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("firetv.json");
    let fake = FakeAdb::new();
    fake.set(|f| f.authorized = false);
    let mut lines = Vec::new();
    let e = || Some(env(&[("EVE_ADB", "adb")]));
    assert_eq!(run_cli(&["pair"], &p, Some(fake.clone()), e(), &mut lines), 1);
    assert!(lines.last().unwrap().contains("Allow USB debugging") && !p.exists());
    fake.set(|f| {
        f.authorized = true;
        f.mdns_rows.clear();
    });
    assert_eq!(run_cli(&["pair", "--host", HOST], &p, Some(fake.clone()), e(), &mut lines), 0);
    assert_eq!(load(Some(&p))["mdns"], json!("adb-TESTSERIAL0001")); // from ro.serialno when mDNS is quiet
}

#[test]
fn test_cli_keys_volume_and_open() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("firetv.json");
    save(&cfg(HOST, Some(MDNS)), Some(&p)).unwrap();
    let fake = FakeAdb::new();
    let mut lines = Vec::new();
    let e = || Some(env(&[("EVE_ADB", "adb")]));
    assert_eq!(run_cli(&["key", "down", "--times", "2"], &p, Some(fake.clone()), e(), &mut lines), 0);
    assert_eq!(run_cli(&["volume", "up"], &p, Some(fake.clone()), e(), &mut lines), 0);
    assert_eq!(run_cli(&["open", "prime", "video"], &p, Some(fake.clone()), e(), &mut lines), 0);
    assert_eq!(run_cli(&["key", "power"], &p, Some(fake.clone()), e(), &mut lines), 1);
    assert_eq!(lines[..3], ["Pressed down 2 times.", "TV volume 54%.", "Opened Prime Video."]);
    assert!(lines[3].starts_with("Unknown key 'power'. Keys: up, down,"));
}

#[test]
fn test_cli_numbers_read_like_argparse() {
    // argparse: "-5" is a positional (no option looks like a number), and `type=int` is `int()`
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("firetv.json");
    save(&cfg(HOST, Some(MDNS)), Some(&p)).unwrap();
    let fake = FakeAdb::new();
    let mut lines = Vec::new();
    let e = || Some(env(&[("EVE_ADB", "adb")]));
    assert_eq!(run_cli(&["volume", "-5"], &p, Some(fake.clone()), e(), &mut lines), 1);
    assert_eq!(lines, vec![s("Give a level 0-100, up or down.")]);
    lines.clear();
    assert_eq!(run_cli(&["key", "down", "--times", "-3"], &p, Some(fake.clone()), e(), &mut lines), 0);
    assert_eq!(run_cli(&["key", "up", "--times", " +2 "], &p, Some(fake.clone()), e(), &mut lines), 0);
    assert_eq!(lines, vec![s("Pressed down."), s("Pressed up 2 times.")]);
    assert_eq!(run_cli(&["key", "up", "--times", "x"], &p, Some(fake.clone()), e(), &mut lines), 2);
}

// ---- guards ----

const SOURCE: &str = include_str!("lib.rs");

#[test]
fn test_never_a_raw_binder_call() {
    let needle = ["service", "call"].join(" ");
    assert!(!SOURCE.to_lowercase().contains(&needle));
}

#[tokio::test]
async fn test_never_the_power_toggle_key() {
    let toggle = Regex::new(r#"keyevent['",\s]+26\b"#).unwrap();
    assert!(!toggle.is_match(SOURCE) && !SOURCE.contains(&["KEYCODE", "POWER"].join("_")));
    let (t, fake) = tv();
    fake.set(|f| {
        f.wake = s("Asleep");
        f.display = s("OFF");
    });
    for action in ["power_on", "power_off", "power_on", "home", "mute", "unmute"] {
        t.control(action, ca()).await;
    }
    let sent = fake.sent().join(" | ");
    assert!(!Regex::new(r"keyevent(?: \d+)* 26(?: |$)").unwrap().is_match(&sent));
    assert!(sent.contains("input keyevent 224") && sent.contains("input keyevent 223"));
}

// ---- the Mini App's remote (FireTV.panel) ----

#[tokio::test]
async fn test_panel_status_carries_the_state_the_line_and_the_app_list() {
    let (t, fake) = tv();
    fake.set(|f| f.focus = s(YT_FOCUS));
    let out = t.panel(&json!({"action": "status"})).await;
    assert!(ok(&out) && out["power"] == json!("on") && out["app"] == json!("YouTube") && out["volume"] == json!(44));
    assert!(out["muted"] == json!(false) && out["now"] == json!("TV on — YouTube, volume 44%"));
    assert_eq!(out["apps"], app_names());
    assert_eq!(out["apps"].as_array().unwrap()[..4], [json!("YouTube"), json!("Netflix"), json!("Prime Video"), json!("Disney+")]);
    assert!(fake.sent().is_empty()); // a status sends nothing to the TV
}

#[tokio::test]
async fn test_panel_no_action_is_a_status() {
    let (t, fake) = tv();
    assert!(t.panel(&json!({})).await["now"] == json!("TV on — home, volume 44%") && fake.sent().is_empty());
}

#[tokio::test]
async fn test_panel_keys_go_through_control_and_answer_with_fresh_state() {
    for (action, sent) in [
        ("up", "input keyevent 19"),
        ("down", "input keyevent 20"),
        ("left", "input keyevent 21"),
        ("right", "input keyevent 22"),
        ("select", "input keyevent 23"),
        ("back", "input keyevent 4"),
        ("home", "input keyevent 3"),
        ("menu", "input keyevent 82"),
        ("play_pause", "cmd media_session dispatch play-pause"),
        ("rewind", "cmd media_session dispatch rewind"),
        ("fast_forward", "cmd media_session dispatch fast-forward"),
    ] {
        let (t, fake) = tv();
        let out = t.panel(&json!({"action": action})).await;
        assert!(ok(&out) && !line(&out).is_empty() && out["now"] == json!("TV on — home, volume 44%"));
        assert_eq!(fake.sent(), vec![sent]);
        assert_eq!(fake.shells().last().unwrap(), STATE_SCRIPT); // the state is read AFTER the key
    }
}

#[tokio::test]
async fn test_panel_dpad_taps_with_a_count_are_one_input_call_capped() {
    let (t, fake) = tv();
    assert_eq!(line(&t.panel(&json!({"action": "down", "repeat": 3})).await), "Pressed down 3 times.");
    t.panel(&json!({"action": "right", "repeat": 99})).await;
    t.panel(&json!({"action": "play_pause", "repeat": 5})).await; // only the d-pad repeats
    assert_eq!(
        fake.sent(),
        vec![
            s("input keyevent 20 20 20"),
            format!("input keyevent {}", vec!["22"; MAX_REPEAT as usize].join(" ")),
            s("cmd media_session dispatch play-pause"),
        ]
    );
}

#[tokio::test]
async fn test_panel_power_on_and_off() {
    let (t, fake) = tv();
    fake.set(|f| {
        f.wake = s("Asleep");
        f.display = s("OFF");
    });
    let on = t.panel(&json!({"action": "power_on"})).await;
    assert!(ok(&on) && on["power"] == json!("on") && on["now"].as_str().unwrap().starts_with("TV on"));
    let off = t.panel(&json!({"action": "power_off"})).await;
    assert!(ok(&off) && off["power"] == json!("standby") && line(&off) == "TV off.");
    assert_eq!(fake.sent(), vec![format!("input keyevent {KEYCODE_WAKEUP}"), format!("input keyevent {KEYCODE_SLEEP}")]);
}

#[tokio::test]
async fn test_panel_volume_from_the_slider_and_the_buttons() {
    let (t, fake) = tv();
    fake.set(|f| {
        f.max_index = 15;
        f.volume = Some(6);
    });
    let out = t.panel(&json!({"action": "volume", "volume": 60})).await;
    assert!(ok(&out) && out["volume"] == json!(60) && out["now"] == json!("TV on — home, volume 60%"));
    assert_eq!(fake.sent(), vec!["cmd media_session volume --stream 3 --set 9"]);
    fake.set(|f| {
        f.max_index = 100;
        f.volume = Some(44);
    });
    assert_eq!(t.panel(&json!({"action": "volume_change", "change": 10})).await["volume"], json!(54));
    assert_eq!(t.panel(&json!({"action": "volume_change", "change": -10})).await["volume"], json!(44));
    assert_eq!(t.panel(&json!({"action": "volume", "volume": 250})).await["volume"], json!(100)); // clamped
}

#[tokio::test]
async fn test_panel_refuses_what_a_remote_card_does_not_send() {
    for (body, want) in [
        (json!({"action": "volume"}), "What volume?"),
        (json!({"action": "volume", "volume": true}), "What volume?"),
        (json!({"action": "volume", "volume": "60"}), "What volume?"),
        (json!({"action": "volume_change"}), "Louder or quieter?"),
        (json!({"action": "volume_change", "change": 0}), "Louder or quieter?"),
        (json!({"action": "open_app", "app": "com.example.coolgame"}), "That app isn't on the remote."),
        (json!({"action": "open_app"}), "That app isn't on the remote."),
        (json!({"action": "type_text", "text": "hello"}), "Not something the TV remote does."),
        (json!({"action": "youtube_search", "query": "cats"}), "Not something the TV remote does."),
        (json!({"action": "next"}), "Not something the TV remote does."),
    ] {
        let (t, fake) = tv();
        let out = t.panel(&body).await;
        assert!(not_ok(&out) && line(&out) == want && !out["apps"].as_array().unwrap().is_empty(), "{body}");
        assert!(fake.calls().is_empty()); // not even a connect
    }
}

#[tokio::test]
async fn test_panel_mute_and_unmute_answer_with_the_read_back() {
    let (t, fake) = tv();
    let out = t.panel(&json!({"action": "mute"})).await;
    assert!(ok(&out) && out["muted"] == json!(true) && out["now"] == json!("TV on — home, volume 44% (muted)"));
    assert_eq!(line(&t.panel(&json!({"action": "mute"})).await), "The TV is already muted.");
    let out = t.panel(&json!({"action": "unmute"})).await;
    assert!(ok(&out) && out["muted"] == json!(false));
    assert_eq!(fake.sent(), vec![format!("input keyevent {KEYCODE_VOLUME_MUTE}"); 2]);
}

#[tokio::test]
async fn test_panel_open_app_by_the_tables_name_only() {
    let (t, fake) = tv();
    let out = t.panel(&json!({"action": "open_app", "app": "Disney+"})).await;
    assert!(ok(&out) && line(&out) == "Opened Disney+." && out["app"] == json!("Disney+"));
    assert_eq!(fake.sent(), vec!["am start -n com.disney.disneyplus/com.bamtechmedia.dominguez.main.MainActivity"]);
}

#[tokio::test]
async fn test_panel_a_gone_tv_says_so_without_a_second_wait() {
    let (t, fake) = tv();
    fake.set(|f| {
        f.at = s("192.0.2.99"); // the TV answers nowhere we know
        f.mdns_rows.clear();
    });
    let out = t.panel(&json!({"action": "up"})).await;
    assert_eq!(Value::Object(out), json!({"ok": false, "line": UNREACHABLE_LINE, "apps": app_names()}));
    let connects = fake.calls().iter().filter(|c| c.get(1).map(String::as_str) == Some("connect")).count();
    assert_eq!(connects, 2); // the press's own try and its one retry; no state read
    let status = t.panel(&json!({"action": "status"})).await;
    assert!(not_ok(&status) && status["now"] == Value::Null && line(&status) == UNREACHABLE_LINE);
}

#[tokio::test]
async fn test_panel_a_failed_action_still_shows_the_fresh_state() {
    let (t, fake) = tv();
    fake.set(|f| f.volume = None); // the volume can't be read: nothing is set
    let out = t.panel(&json!({"action": "volume_change", "change": 10})).await;
    assert!(not_ok(&out) && line(&out) == "I couldn't read the TV's volume, so I didn't change it.");
    assert!(out["power"] == json!("on") && out["now"] == json!("TV on — home, volume unknown"));
    assert!(fake.sent().is_empty());
}

// ---- the stale adb server (2026-10-08: "No route to host" for hours while the TV was on) ----

struct Stale {
    calls: Mutex<Vec<Vec<String>>>,
    restarted: AtomicBool,
}

fn stale_server_tv(probe_answers: bool, clock: Arc<Mutex<f64>>) -> (FireTV, Arc<Stale>) {
    let st = Arc::new(Stale { calls: Mutex::new(Vec::new()), restarted: AtomicBool::new(false) });
    let s2 = st.clone();
    let runner = move |argv: Vec<String>, _t: f64| {
        let st = s2.clone();
        async move {
            st.calls.lock().push(argv.clone());
            if argv[1] == "kill-server" {
                st.restarted.store(true, Ordering::SeqCst);
                return (0, String::new());
            }
            if argv[1] == "connect" {
                if st.restarted.load(Ordering::SeqCst) {
                    return (0, format!("connected to {SERIAL}"));
                }
                return (0, format!("failed to connect to '{SERIAL}': No route to host"));
            }
            if argv[1] == "mdns" {
                return (0, s("List of discovered mdns services\n"));
            }
            if argv[1] == "-s" && argv[3] == "get-state" {
                return (0, s("device\n"));
            }
            (0, String::new())
        }
    };
    let t = FireTV::new(cfg(HOST, Some(MDNS)))
        .with_runner(Arc::new(runner))
        .with_adb("adb")
        .with_sleep(no_sleep())
        .with_probe(Arc::new(move |_h: String, _p: i64, _t: f64| async move { probe_answers }))
        .with_clock(Arc::new(move || *clock.lock()));
    (t, st)
}

fn verbs(st: &Stale) -> Vec<String> {
    st.calls.lock().iter().map(|c| c[1].clone()).collect()
}

#[tokio::test]
async fn test_a_stale_adb_server_is_restarted_once_when_the_tv_port_answers() {
    let (t, st) = stale_server_tv(true, Arc::new(Mutex::new(1000.0)));
    assert!(t.ensure().await.ok);
    let v = verbs(&st);
    assert!(v.iter().filter(|x| *x == "kill-server").count() == 1 && v.contains(&s("start-server")));
    let pos = |w: &str| v.iter().position(|x| x == w).unwrap();
    assert!(pos("kill-server") > pos("disconnect")); // only after the normal recovery failed
}

#[tokio::test]
async fn test_the_adb_server_is_left_alone_when_the_tv_itself_is_gone() {
    let (t, st) = stale_server_tv(false, Arc::new(Mutex::new(1000.0)));
    let r = t.ensure().await;
    assert!(!r.ok && r.why == "unreachable");
    assert!(!verbs(&st).iter().any(|v| v == "kill-server" || v == "start-server"));
}

#[tokio::test]
async fn test_the_adb_server_is_restarted_at_most_once_per_ten_minutes() {
    let clock = Arc::new(Mutex::new(1000.0));
    let (t, st) = stale_server_tv(true, clock.clone());
    assert!(t.ensure().await.ok);
    st.restarted.store(false, Ordering::SeqCst); // the server goes stale again
    t.ready.store(false, Ordering::SeqCst);
    *clock.lock() += 60.0;
    assert!(!t.ensure().await.ok);
    assert_eq!(verbs(&st).iter().filter(|x| *x == "kill-server").count(), 1);
    *clock.lock() += ADB_RESTART_EVERY;
    assert!(t.ensure().await.ok);
    assert_eq!(verbs(&st).iter().filter(|x| *x == "kill-server").count(), 2);
}

#[tokio::test]
async fn test_an_unauthorized_tv_never_restarts_the_server() {
    let t = tv_runner(cfg(HOST, None), |argv| {
        if argv[1] == "connect" {
            return (0, format!("failed to authenticate to {SERIAL}"));
        }
        (0, String::new())
    })
    .with_probe(Arc::new(|_h: String, _p: i64, _t: f64| async { panic!("probed an unauthorized TV") }));
    assert_eq!(t.ensure().await.why, "unauthorized");
}

#[tokio::test]
async fn test_the_real_probe_tells_an_open_port_from_a_closed_one() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = i64::from(listener.local_addr().unwrap().port());
    let opened = tcp_probe("127.0.0.1", port, 1.0).await;
    drop(listener);
    let closed = tcp_probe("127.0.0.1", port, 1.0).await;
    assert_eq!((opened, closed), (true, false));
}

// ---- beyond the Python file: parity details the port depends on ----

#[test]
fn test_shlex_quote_like_python() {
    assert_eq!(shlex_quote(""), "''");
    assert_eq!(shlex_quote("com.netflix.ninja/.MainActivity"), "com.netflix.ninja/.MainActivity");
    assert_eq!(shlex_quote("it's"), "'it'\"'\"'s'");
    assert_eq!(shlex_quote("a b"), "'a b'");
    assert_eq!(sh(&["echo", "@@"]), "echo @@");
}

#[test]
fn test_quote_plus_like_python() {
    assert_eq!(quote_plus("a b&c/d~é"), "a+b%26c%2Fd~%C3%A9");
}

#[test]
fn test_section_reads_like_the_python_regex() {
    assert_eq!(section("@@POWER\nA\n@@FOCUS\nB\n@@END\n", "POWER"), "A");
    assert_eq!(section("@@POWER\n\n\nA\n@@FOCUS\n", "POWER"), "A");
    assert_eq!(section("@@POWER x\n@@POWER\nA", "POWER"), "A");
    assert_eq!(section("@@FOCUS\n", "POWER"), "");
    assert_eq!(section("@@AUDIO\nX@@lower\nY", "AUDIO"), "X@@lower\nY");
}

#[test]
fn test_load_keeps_other_keys_and_coerces_the_port() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("firetv.json");
    std::fs::write(&p, r#"{"model": "AFT", "host": " 192.0.2.40 ", "port": "6000", "mdns": null}"#).unwrap();
    let c = load(Some(&p));
    assert_eq!(Value::Object(c.clone()), json!({"model": "AFT", "host": HOST, "port": 6000, "mdns": ""}));
    assert_eq!(c.keys().collect::<Vec<_>>(), ["model", "host", "port", "mdns"]);
    std::fs::write(&p, r#"{"host": "h", "port": "x"}"#).unwrap();
    assert_eq!(load(Some(&p))["port"], json!(5555));
    std::fs::write(&p, "not json").unwrap();
    assert!(load(Some(&p)).is_empty());
}

#[test]
fn test_save_writes_what_python_writes() {
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path().join("firetv.json");
    let mut c = cfg(HOST, Some(MDNS));
    c.insert("model".into(), json!("AFTTEST1"));
    save(&c, Some(&p)).unwrap();
    assert_eq!(
        std::fs::read_to_string(&p).unwrap(),
        "{\n  \"host\": \"192.0.2.40\",\n  \"port\": 5555,\n  \"mdns\": \"adb-TESTSERIAL0001\",\n  \"model\": \"AFTTEST1\"\n}"
    );
    assert!(!dir.path().join("firetv.tmp").exists());
}

#[test]
fn test_find_app_by_key_and_package_names() {
    assert_eq!(find_app("the Disney+ app").unwrap().name, "Disney+");
    assert_eq!(find_app("My Live TV").unwrap().name, "Live TV");
    assert!(find_app("").is_none());
    assert_eq!(YOUTUBE.package(), "com.amazon.firetv.youtube");
    assert_eq!(app_name("com.amazon.tv.launcher", "com.amazon.tv.launcher.ui.SettingsActivity"), "Settings");
    assert_eq!(app_name("com.unknown.thing", ""), "com.unknown.thing");
}

#[test]
fn test_instructions_text_is_the_pythons() {
    assert_eq!(
        INSTRUCTIONS_NO_SCREENSHOT.as_str(),
        "The owner's Fire TV.\nWhat you can do: firetv_control turns it on or off, presses the remote's keys, plays \
         and pauses, sets the volume,\nmutes, opens an app by name, searches YouTube and types into a focused search \
         box. firetv_status reads power, the\napp in front and the volume. \"Turn it up\" is volume_change 10, \"a \
         bit\" 5.\nVerify: power, volume and mute are read back, so say the result you got. The d-pad, media keys, \
         opening apps and\ntyping are not, so say what you pressed or opened. Don't guess what is on the \
         screen.\nWhen an app is already in front and the owner asks for another, go ahead, but say what you \
         interrupted.\nLimits: HDMI inputs can't be switched. In deep standby the TV can't be reached: say to use \
         the remote or Alexa.\nSearch works in YouTube only."
    );
}

#[tokio::test]
async fn test_capture_returns_error_words_on_failure() {
    let (rc, out) = adb_capture(argv(&["/bin/sh", "-c", "printf png; printf oops >&2; exit 3"]), 5.0).await;
    assert_eq!((rc, out), (3, b"oopspng".to_vec()));
    let (rc, out) = adb_capture(argv(&["/bin/sh", "-c", "printf png; printf oops >&2"]), 5.0).await;
    assert_eq!((rc, out), (0, b"png".to_vec()));
    let (rc, out) = adb_runner(argv(&["/bin/sh", "-c", "echo a; echo b >&2; echo c"]), 5.0).await;
    assert_eq!((rc, out.as_str()), (0, "a\nb\nc\n"));
}
