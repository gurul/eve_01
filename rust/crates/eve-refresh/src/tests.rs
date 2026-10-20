//! Port of tests/test_refresh.py: the weekly reminder's dates, message and LaunchAgent. The goldens below were
//! printed by the Python module (HOME=/Users/owner, sys.executable=/venv/bin/python, TZ=UTC).

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, TimeDelta, TimeZone, Utc};
use eve_brain::notify::{NotifyConfig, PostFn};
use eve_claude_home::HomeEnv;
use eve_service::{Completed, FullPaths, Host, Launch, Launchd};

use super::*;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 16, 18, 0, 0).unwrap()
}

/// `_fake_security`: `security cms -D` answering a profile whose ExpirationDate is `expiry`.
fn fake_security(expiry: plist::Date) -> impl Fn(&[String]) -> io::Result<Ran> {
    move |cmd: &[String]| {
        assert_eq!(&cmd[..3], ["security", "cms", "-D"]);
        let mut dict = plist::Dictionary::new();
        dict.insert("ExpirationDate".into(), plist::Value::Date(expiry));
        let mut out = Vec::new();
        plist::Value::Dictionary(dict).to_writer_xml(&mut out).unwrap();
        Ok(Ran { returncode: 0, stdout: out })
    }
}

fn date(dt: DateTime<Utc>) -> plist::Date {
    std::time::SystemTime::from(dt).into()
}

#[test]
fn test_app_expiry_reads_the_profile() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("embedded.mobileprovision"), b"x").unwrap();
    let expiry = Utc.with_ymd_and_hms(2026, 10, 23, 18, 40, 0).unwrap();
    let got = app_expiry(tmp.path(), &fake_security(date(expiry))).unwrap();
    assert_eq!(got, Some(expiry));
}

#[test]
fn test_app_expiry_none_without_a_build() {
    let tmp = tempfile::tempdir().unwrap();
    let run = fake_security(date(Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap()));
    assert_eq!(app_expiry(tmp.path(), &run).unwrap(), None);
}

#[test]
fn app_expiry_reads_the_real_xml_security_prints() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("embedded.mobileprovision"), b"x").unwrap();
    let xml = b"<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \
\"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>AppIDName</key>\n\t\
<string>EveLink</string>\n\t<key>ExpirationDate</key>\n\t<date>2026-10-23T18:40:12Z</date>\n</dict>\n</plist>\n";
    let run = |_: &[String]| Ok(Ran { returncode: 0, stdout: xml.to_vec() });
    assert_eq!(app_expiry(tmp.path(), &run).unwrap(), Some(Utc.with_ymd_and_hms(2026, 10, 23, 18, 40, 12).unwrap()));
}

#[test]
fn app_expiry_unknown_when_security_fails_or_prints_garbage() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("embedded.mobileprovision"), b"x").unwrap();
    let failed = |_: &[String]| Ok(Ran { returncode: 1, stdout: Vec::new() });
    assert_eq!(app_expiry(tmp.path(), &failed).unwrap(), None);
    let garbage = |_: &[String]| Ok(Ran { returncode: 0, stdout: b"not a plist".to_vec() });
    assert_eq!(app_expiry(tmp.path(), &garbage).unwrap(), None);
    let mut out = Vec::new();
    plist::Value::Array(vec![]).to_writer_xml(&mut out).unwrap();
    let not_a_dict = move |_: &[String]| Ok(Ran { returncode: 0, stdout: out.clone() });
    assert_eq!(app_expiry(tmp.path(), &not_a_dict).unwrap(), None);
    let no_date = |_: &[String]| {
        let mut out = Vec::new();
        let mut d = plist::Dictionary::new();
        d.insert("ExpirationDate".into(), plist::Value::String("soon".into()));
        plist::Value::Dictionary(d).to_writer_xml(&mut out).unwrap();
        Ok(Ran { returncode: 0, stdout: out })
    };
    assert_eq!(app_expiry(tmp.path(), &no_date).unwrap(), None);
    // subprocess.run raising (no `security`) is not swallowed
    let missing = |_: &[String]| Err(io::Error::new(io::ErrorKind::NotFound, "security"));
    assert!(app_expiry(tmp.path(), &missing).is_err());
}

#[test]
fn test_google_expiry_is_seven_days_after_sign_in() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("google-token.json");
    std::fs::write(&p, format!(r#"{{"refresh_token": "r", "signed_in_at": {}.0}}"#, now().timestamp())).unwrap();
    assert_eq!(google_expiry(&p).unwrap(), Some(now() + TimeDelta::days(7)));
}

#[test]
fn test_google_expiry_unknown_without_a_stamp_or_file() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("google-token.json");
    assert_eq!(google_expiry(&p).unwrap(), None);
    std::fs::write(&p, r#"{"refresh_token": "r"}"#).unwrap();
    assert_eq!(google_expiry(&p).unwrap(), None);
}

#[test]
fn google_expiry_reads_the_stamp_as_python_float_does() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("google-token.json");
    let write = |body: &str| std::fs::write(&p, body).unwrap();
    // a fractional stamp, rounded to the microsecond
    write(r#"{"signed_in_at": 1760637600.25}"#);
    assert_eq!(
        google_expiry(&p).unwrap(),
        Some(DateTime::from_timestamp(1760637600, 250_000_000).unwrap() + TimeDelta::days(7))
    );
    // a numeric string is float()-able; underscores between digits too
    write(r#"{"signed_in_at": " 1_760_637_600 "}"#);
    assert_eq!(google_expiry(&p).unwrap(), Some(DateTime::from_timestamp(1760637600, 0).unwrap() + TimeDelta::days(7)));
    // falsy values are "never stamped"
    for falsy in ["0", "null", "false", "\"\"", "[]", "{}"] {
        write(&format!(r#"{{"signed_in_at": {falsy}}}"#));
        assert_eq!(google_expiry(&p).unwrap(), None, "{falsy}");
    }
    // a string float() refuses is a ValueError: unknown
    for bad in ["yesterday", "+-1", "1__0", "_1", "1_", "e5", "."] {
        write(&format!(r#"{{"signed_in_at": "{bad}"}}"#));
        assert_eq!(google_expiry(&p).unwrap(), None, "{bad}");
    }
    write(r#"{"signed_in_at": "-1.5e3"}"#);
    assert_eq!(google_expiry(&p).unwrap(), Some(DateTime::from_timestamp(-1500, 0).unwrap() + TimeDelta::days(7)));
    // not JSON, not UTF-8: ValueError, unknown
    write("{");
    assert_eq!(google_expiry(&p).unwrap(), None);
    std::fs::write(&p, [0xff, 0xfe]).unwrap();
    assert_eq!(google_expiry(&p).unwrap(), None);
}

#[test]
fn google_expiry_raises_where_python_does() {
    let tmp = tempfile::tempdir().unwrap();
    let p = tmp.path().join("google-token.json");
    // .get on a list: AttributeError, outside the except
    std::fs::write(&p, "[1]").unwrap();
    assert!(google_expiry(&p).unwrap_err().to_string().starts_with("AttributeError"));
    // float([1]): TypeError
    std::fs::write(&p, r#"{"signed_in_at": [1]}"#).unwrap();
    assert!(google_expiry(&p).unwrap_err().to_string().starts_with("TypeError"));
    // fromtimestamp out of range
    std::fs::write(&p, r#"{"signed_in_at": 1e300}"#).unwrap();
    assert!(google_expiry(&p).is_err());
    std::fs::write(&p, r#"{"signed_in_at": "nan"}"#).unwrap();
    assert!(google_expiry(&p).is_err());
}

#[test]
fn test_message_names_both_steps_and_how_long_is_left() {
    let text = message(now(), Some(now() + TimeDelta::days(1) + TimeDelta::hours(2)), Some(now() - TimeDelta::hours(1)));
    assert!(text.contains("build.sh --install") && text.contains("(in 1 day)"));
    assert!(text.contains("apps login google") && text.contains("expired"));
    assert!(message(now(), None, None).contains("date unknown"));
}

#[test]
fn message_matches_python_byte_for_byte() {
    let got = message_in(now(), Some(now() + TimeDelta::days(1) + TimeDelta::hours(2)), Some(now() - TimeDelta::hours(1)), &Utc);
    assert_eq!(
        got,
        "Weekly refresh, on the Mac:\n\
1. Eve Link expires Sat Oct 17, 8:00 PM (in 1 day): sh ios/EveLink/build.sh --install (phone unlocked)\n\
2. Google expired Fri Oct 16: eve-bridge apps login google, then restart the daemon"
    );
    let got = message_in(now(), Some(now() + TimeDelta::days(6) + TimeDelta::hours(23)), Some(now() + TimeDelta::hours(3)), &Utc);
    assert_eq!(
        got,
        "Weekly refresh, on the Mac:\n\
1. Eve Link expires Fri Oct 23, 5:00 PM (in 6 days): sh ios/EveLink/build.sh --install (phone unlocked)\n\
2. Google expires Fri Oct 16, 9:00 PM (today): eve-bridge apps login google, then restart the daemon"
    );
    assert_eq!(when_in(None, now(), &Utc), "date unknown");
}

#[test]
fn mac_notification_quotes_as_json_dumps() {
    let argv = mac_notification_argv("é \"x\" 😀\n");
    assert_eq!(argv[..2], ["osascript", "-e"]);
    let want = ["display notification \"", "\\u00e9 \\\"x\\\" \\ud83d\\ude00\\n", "\" with title \"eve: refresh\""].concat();
    assert_eq!(argv[2], want);
}

#[test]
fn remind_shows_on_the_mac_and_posts_when_enabled() {
    let ran: Mutex<Vec<Vec<String>>> = Mutex::new(Vec::new());
    let run = |argv: &[String]| {
        ran.lock().unwrap().push(argv.to_vec());
        Ok(Ran::default())
    };
    type Posts = Arc<Mutex<Vec<(String, Vec<u8>)>>>;
    let posted: Posts = Arc::default();
    let sink = posted.clone();
    let post: PostFn = Arc::new(move |url: &str, body: &[u8], _h: &[(String, String)]| {
        sink.lock().unwrap().push((url.to_string(), body.to_vec()));
        Ok(200)
    });
    let on = NotifyConfig { enabled: true, base: "https://ntfy.example".into(), preview: true };
    assert!(remind_with("hi", &run, &on, &|| "tok".into(), &post).unwrap());
    assert_eq!(ran.lock().unwrap()[0][0], "osascript");
    assert_eq!(posted.lock().unwrap()[0].1, b"hi");
    assert!(posted.lock().unwrap()[0].0.starts_with("https://ntfy.example/eve-"));

    // off: the Mac still shows it, the phone post never happens and the token is never read
    let off = NotifyConfig { enabled: false, ..on.clone() };
    let token = || -> String { panic!("the token is not read when notifications are off") };
    assert!(!remind_with("hi", &run, &off, &token, &post).unwrap());
    assert_eq!(ran.lock().unwrap().len(), 2);
    assert_eq!(posted.lock().unwrap().len(), 1);

    // not paired: no token, no post
    assert!(!remind_with("hi", &run, &on, &String::new, &post).unwrap());
    assert_eq!(posted.lock().unwrap().len(), 1);
}

// ---- the LaunchAgent ---------------------------------------------------------------------------------------------

#[derive(Default)]
struct FakeHost {
    launchctl: bool,
    calls: Mutex<Vec<Vec<String>>>,
    out: Mutex<Vec<String>>,
    err: Mutex<Vec<String>>,
    load_rc: i32,
}

impl Host for FakeHost {
    fn which(&self, _name: &str) -> Option<PathBuf> {
        self.launchctl.then(|| PathBuf::from("/bin/launchctl"))
    }
    fn run(&self, argv: &[String]) -> io::Result<Completed> {
        assert_eq!(argv[0], "launchctl");
        self.calls.lock().unwrap().push(argv.to_vec());
        let rc = if argv[1] == "load" { self.load_rc } else { 0 };
        Ok(Completed::new(rc, "", if rc != 0 { "boom\n" } else { "" }))
    }
    fn print(&self, line: &str) {
        self.out.lock().unwrap().push(line.to_string());
    }
    fn eprint(&self, line: &str) {
        self.err.lock().unwrap().push(line.to_string());
    }
    fn getuid(&self) -> io::Result<u32> {
        Ok(501)
    }
}

fn launchd_at(home: &Path, launch: Launch, host: Arc<FakeHost>) -> Launchd {
    Launchd {
        env: HomeEnv { claude_config_dir: None, claude_config_dirs: None, home: Some(home.as_os_str().to_owned()) },
        full: FullPaths::under(home),
        label: eve_service::service_launchd::LABEL.to_string(),
        launch,
        host,
    }
}

const PLIST_GOLDEN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>EnvironmentVariables</key>
	<dict>
		<key>HOME</key>
		<string>/Users/owner</string>
		<key>PATH</key>
		<string>/usr/bin:/bin:/usr/sbin:/sbin</string>
	</dict>
	<key>Label</key>
	<string>com.github.eve-bridge.refresh-reminder</string>
	<key>ProgramArguments</key>
	<array>
		<string>/venv/bin/python</string>
		<string>-m</string>
		<string>eve_bridge.cli</string>
		<string>refresh</string>
		<string>remind</string>
	</array>
	<key>StandardErrorPath</key>
	<string>/Users/owner/Library/Logs/eve-refresh-reminder.log</string>
	<key>StandardOutPath</key>
	<string>/Users/owner/Library/Logs/eve-refresh-reminder.log</string>
	<key>StartCalendarInterval</key>
	<dict>
		<key>Hour</key>
		<integer>9</integer>
		<key>Minute</key>
		<integer>0</integer>
		<key>Weekday</key>
		<integer>5</integer>
	</dict>
</dict>
</plist>
"#;

#[test]
fn test_plist_runs_the_venv_python_weekly() {
    let l = launchd_at(Path::new("/Users/owner"), Launch::Python("/venv/bin/python".into()), Arc::default());
    let bytes = build_plist(&l).unwrap();
    let plist = plist::Value::from_reader(std::io::Cursor::new(&bytes)).unwrap();
    let d = plist.as_dictionary().unwrap();
    let args: Vec<&str> =
        d.get("ProgramArguments").unwrap().as_array().unwrap().iter().map(|v| v.as_string().unwrap()).collect();
    assert_eq!(args, ["/venv/bin/python", "-m", "eve_bridge.cli", "refresh", "remind"]);
    let cal = d.get("StartCalendarInterval").unwrap().as_dictionary().unwrap();
    let int = |k: &str| cal.get(k).unwrap().as_signed_integer().unwrap();
    assert_eq!((int("Weekday"), int("Hour"), int("Minute")), (5, 9, 0));
    assert_eq!(cal.len(), 3);
    assert!(d.get("RunAtLoad").is_none());
    assert_eq!(String::from_utf8(bytes).unwrap(), PLIST_GOLDEN);
}

#[test]
fn plist_runs_the_rust_binary_natively() {
    let l = launchd_at(Path::new("/Users/owner"), Launch::Native("/opt/eve/eve-bridge".into()), Arc::default());
    assert_eq!(program_arguments(&l.launch), ["/opt/eve/eve-bridge", "refresh", "remind"]);
}

#[test]
fn schedule_writes_and_loads_the_agent_then_unschedule_removes_it() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(FakeHost { launchctl: true, ..Default::default() });
    let l = launchd_at(tmp.path(), Launch::Python("/py".into()), host.clone());
    let path = tmp.path().join("Library/LaunchAgents/com.github.eve-bridge.refresh-reminder.plist");
    assert_eq!(plist_path(&l).unwrap(), path);

    assert_eq!(schedule(&l).unwrap(), 0);
    assert_eq!(std::fs::read(&path).unwrap(), build_plist(&l).unwrap());
    let p = path.to_string_lossy().into_owned();
    assert_eq!(
        *host.calls.lock().unwrap(),
        [vec!["launchctl".to_string(), "unload".into(), p.clone()], vec!["launchctl".into(), "load".into(), "-w".into(), p.clone()]]
    );
    assert_eq!(host.out.lock().unwrap()[0], format!("Scheduled: every Friday at 09:00 ({p})."));

    assert_eq!(unschedule(&l).unwrap(), 0);
    assert!(!path.exists());
    assert_eq!(host.calls.lock().unwrap()[2], ["launchctl", "unload", p.as_str()]);
    assert_eq!(host.out.lock().unwrap()[1], format!("Removed: {p}"));

    assert_eq!(unschedule(&l).unwrap(), 0);
    assert_eq!(host.out.lock().unwrap()[2], "Not scheduled; nothing to do.");
}

#[test]
fn schedule_without_launchctl_or_with_a_failed_load_is_2() {
    let tmp = tempfile::tempdir().unwrap();
    let host = Arc::new(FakeHost::default());
    let l = launchd_at(tmp.path(), Launch::Python("/py".into()), host.clone());
    assert_eq!(schedule(&l).unwrap(), 2);
    assert_eq!(host.err.lock().unwrap()[0], "eve-bridge: `launchctl` not found on PATH");
    assert!(!plist_path(&l).unwrap().exists());

    let host = Arc::new(FakeHost { launchctl: true, load_rc: 3, ..Default::default() });
    let l = launchd_at(tmp.path(), Launch::Python("/py".into()), host.clone());
    assert_eq!(schedule(&l).unwrap(), 2);
    assert_eq!(host.err.lock().unwrap()[0], "launchctl load failed (3): boom");
    assert!(host.out.lock().unwrap().is_empty());
}

#[test]
fn app_is_the_last_device_build_in_this_checkout() {
    assert!(APP.ends_with("ios/EveLink/build/Build/Products/Debug-iphoneos/EveLink.app"));
    assert!(EVE.join("bridge/src/eve_bridge/refresh.py").exists());
}
