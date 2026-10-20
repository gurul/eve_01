//! Ports of bridge/tests/test_service.py, test_service_lite_plist.py, test_service_systemd.py and
//! test_service_windows.py, plus the plist parts of test_notes_widget.py and test_owner_golden.py.
//!
//! launchctl, systemctl and schtasks never run here: every call goes to [`Fake`], a recorder that answers as the
//! tool would (Python's monkeypatched `subprocess.run` and `shutil.which`), and HOME is a temporary folder.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eve_claude_home::HomeEnv;

use crate::host::{Completed, Host, Launch};
use crate::service::{backend_kind, unsupported_platform_msg, Backend, Location, Service};
use crate::service_launchd::{self as launchd, FullPaths, Launchd, LABEL, LITE_LABEL};
use crate::service_systemd::{self as systemd, Systemd, UNIT_NAME};
use crate::service_windows::{self as win, TaskScheduler};

const PYTHON: &str = "/venv/bin/python3.12";
const UID: u32 = 501;

/// `subprocess.run` and `shutil.which` for the service tools: records each call and answers as the tool would;
/// `listed` is what `launchctl list` shows, a verb in `fail` exits 5.
struct Fake {
    found: Option<&'static str>,
    listed: Vec<String>,
    fail: Vec<String>,
    calls: Mutex<Vec<Vec<String>>>,
    out: Mutex<String>,
    err: Mutex<String>,
}

impl Fake {
    /// The tools are on PATH.
    fn new(listed: &[&str], fail: &[&str]) -> Arc<Fake> {
        Arc::new(Fake {
            found: Some("/bin/tool"),
            listed: listed.iter().map(|s| s.to_string()).collect(),
            fail: fail.iter().map(|s| s.to_string()).collect(),
            calls: Mutex::new(Vec::new()),
            out: Mutex::new(String::new()),
            err: Mutex::new(String::new()),
        })
    }

    /// No tool is found on PATH (the autouse `_no_launchd` fixture).
    fn none() -> Arc<Fake> {
        let mut fake = Fake::new(&[], &[]);
        Arc::get_mut(&mut fake).unwrap().found = None;
        fake
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }

    fn verbs(&self) -> Vec<String> {
        self.calls().into_iter().map(|c| c[1].clone()).collect()
    }

    fn out(&self) -> String {
        std::mem::take(&mut *self.out.lock().unwrap())
    }

    fn err(&self) -> String {
        std::mem::take(&mut *self.err.lock().unwrap())
    }
}

impl Host for Fake {
    fn which(&self, _name: &str) -> Option<PathBuf> {
        self.found.map(PathBuf::from)
    }

    fn run(&self, argv: &[String]) -> io::Result<Completed> {
        assert!(
            ["launchctl", "systemctl", "schtasks"].contains(&argv[0].as_str()),
            "only the service tools run here, not {}",
            argv[0]
        );
        self.calls.lock().unwrap().push(argv.to_vec());
        let verb = argv[1].as_str();
        if argv[0] == "launchctl" && verb == "list" {
            let mut out = String::from("PID\tStatus\tLabel\n");
            for label in &self.listed {
                out.push_str(&format!("-\t0\t{label}\n"));
            }
            return Ok(Completed::new(0, out, ""));
        }
        if self.fail.iter().any(|f| f == verb) {
            return Ok(Completed::new(5, "", "Load failed: 5: Input/output error\n"));
        }
        Ok(Completed::new(0, "", ""))
    }

    fn print(&self, line: &str) {
        let mut out = self.out.lock().unwrap();
        out.push_str(line);
        out.push('\n');
    }

    fn eprint(&self, line: &str) {
        let mut err = self.err.lock().unwrap();
        err.push_str(line);
        err.push('\n');
    }

    fn getuid(&self) -> io::Result<u32> {
        Ok(UID)
    }
}

fn env_at(home: &Path) -> HomeEnv {
    HomeEnv { claude_config_dir: None, claude_config_dirs: None, home: Some(home.as_os_str().to_owned()) }
}

/// The launchd backend in `home`: full eve's paths fixed there, the Python interpreter line.
fn launchd_in(home: &Path, host: Arc<Fake>) -> Launchd {
    Launchd {
        env: env_at(home),
        full: FullPaths::under(home),
        label: LABEL.to_string(),
        launch: Launch::Python(PYTHON.to_string()),
        host,
    }
}

/// A fresh HOME: every lite path follows it.
fn home() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let folder = tmp.path().join("home");
    std::fs::create_dir(&folder).unwrap();
    (tmp, folder)
}

fn parse(data: &[u8]) -> plist::Dictionary {
    match plist::Value::from_reader(io::Cursor::new(data.to_vec())).unwrap() {
        plist::Value::Dictionary(d) => d,
        other => panic!("not a dict: {other:?}"),
    }
}

fn s(d: &plist::Dictionary, key: &str) -> String {
    d.get(key).and_then(|v| v.as_string()).unwrap_or_else(|| panic!("{key} is not a string")).to_string()
}

fn strings(d: &plist::Dictionary, key: &str) -> Vec<String> {
    d.get(key).unwrap().as_array().unwrap().iter().map(|v| v.as_string().unwrap().to_string()).collect()
}

fn env_of(d: &plist::Dictionary) -> HashMap<String, String> {
    d.get("EnvironmentVariables")
        .unwrap()
        .as_dictionary()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_string().unwrap().to_string()))
        .collect()
}

fn lite(b: &Launchd) -> plist::Dictionary {
    parse(&b.build_lite_plist().unwrap())
}

// ==== test_service.py =============================================================================================

#[test]
fn test_plist_parses_as_valid_plist() {
    let (_t, home) = home();
    let data = launchd_in(&home, Fake::none()).build_plist(None).unwrap();
    let parsed = plist::Value::from_reader(io::Cursor::new(data)).unwrap();
    assert!(parsed.as_dictionary().is_some());
}

#[test]
fn test_plist_has_expected_keys() {
    let (_t, home) = home();
    let parsed = parse(&launchd_in(&home, Fake::none()).build_plist(None).unwrap());
    // Required for a user LaunchAgent
    assert_eq!(s(&parsed, "Label"), LABEL);
    assert_eq!(parsed.get("RunAtLoad").unwrap().as_boolean(), Some(true));
    assert_eq!(parsed.get("KeepAlive").unwrap().as_boolean(), Some(true));
    // Interactive is required for CoreBluetooth access from a GUI-session agent
    assert_eq!(s(&parsed, "ProcessType"), "Interactive");
}

#[test]
fn test_plist_program_arguments_point_at_current_interpreter() {
    let (_t, home) = home();
    let parsed = parse(&launchd_in(&home, Fake::none()).build_plist(None).unwrap());
    let args = strings(&parsed, "ProgramArguments");
    assert_eq!(args[0], PYTHON);
    assert_eq!(args[1..], ["-m", "eve_bridge.cli", "daemon"]);
}

#[test]
fn test_plist_program_arguments_name_the_native_binary() {
    let (_t, home) = home();
    let mut b = launchd_in(&home, Fake::none());
    b.launch = Launch::Native("/opt/eve/bin/eve-bridge".into());
    let parsed = parse(&b.build_plist(None).unwrap());
    assert_eq!(strings(&parsed, "ProgramArguments"), ["/opt/eve/bin/eve-bridge", "daemon"]);
    assert_eq!(strings(&parse(&b.build_widget_plist().unwrap()), "ProgramArguments")[1..], ["notes-widget"]);
}

#[test]
fn test_plist_log_paths_redirected() {
    let (_t, home) = home();
    let b = launchd_in(&home, Fake::none());
    let parsed = parse(&b.build_plist(None).unwrap());
    let log = b.full.log_path.to_str().unwrap();
    assert_eq!(s(&parsed, "StandardOutPath"), log);
    assert_eq!(s(&parsed, "StandardErrorPath"), log);
}

#[test]
fn test_plist_env_has_path_and_home() {
    let (_t, home) = home();
    let env = env_of(&parse(&launchd_in(&home, Fake::none()).build_plist(None).unwrap()));
    assert_eq!(env["HOME"], home.to_str().unwrap());
    assert!(env["PATH"].contains("/usr/bin"));
}

#[test]
fn test_backend_name_resolves_per_platform() {
    assert_eq!(backend_kind("darwin").map(|k| k.name()), Some("launchd"));
    assert_eq!(backend_kind("linux").map(|k| k.name()), Some("systemd"));
    assert_eq!(backend_kind("win32").map(|k| k.name()), Some("task-scheduler"));
    // Built from this process, the backend reports the same name.
    for (platform, name) in [("darwin", "launchd"), ("linux", "systemd"), ("win32", "task-scheduler")] {
        assert_eq!(Service::for_platform(platform).unwrap().backend_name(), Some(name));
    }
}

#[test]
fn test_is_installed_false_on_unsupported_platform() {
    for platform in ["freebsd14", "openbsd"] {
        let fake = Fake::new(&[], &[]);
        let svc = Service::with_backend(platform, Backend::for_platform(platform).unwrap(), fake.clone());
        assert!(!svc.is_installed().unwrap());
        assert!(!svc.is_loaded().unwrap());
        assert_eq!(svc.unit_path(), None);
        assert_eq!(svc.log_path(), None);
        assert_eq!(svc.backend_name(), None);
        assert!(fake.calls().is_empty());
    }
}

#[test]
fn unsupported_platform_install_says_so_and_exits_2() {
    let fake = Fake::new(&[], &[]);
    let svc = Service::with_backend("freebsd14", None, fake.clone());
    assert_eq!(svc.install_service(Some("/dev/x")).unwrap(), 2);
    assert_eq!(svc.uninstall_service().unwrap(), 2);
    assert_eq!(
        fake.err(),
        format!("{0}\n{0}\n", "eve-bridge: service install is only supported on macOS, Linux, and Windows (got 'freebsd14').")
    );
    assert_eq!(unsupported_platform_msg("it's"), "eve-bridge: service install is only supported on macOS, Linux, and Windows (got \"it's\").");
    assert!(fake.calls().is_empty());
}

#[test]
fn the_notes_widget_is_macos_only() {
    let (_t, home) = home();
    let fake = Fake::new(&[], &[]);
    let systemd = Systemd {
        env: env_at(&home),
        unit_path: systemd::unit_path_under(&home),
        launch: Launch::Python(PYTHON.into()),
        host: fake.clone(),
    };
    for backend in [None, Some(Backend::Systemd(systemd))] {
        let svc = Service::with_backend("linux", backend, fake.clone());
        assert_eq!(svc.install_notes_widget().unwrap(), 2);
        assert_eq!(svc.uninstall_notes_widget().unwrap(), 2);
    }
    assert_eq!(fake.err(), "eve-bridge: the notes widget is macOS-only (got 'linux').\n".repeat(4));
    assert!(fake.calls().is_empty());
}

#[test]
fn the_dispatcher_reports_each_backends_paths() {
    let (_t, home) = home();
    let fake = Fake::new(&[], &[]);
    let b = launchd_in(&home, fake.clone());
    let svc = Service::with_backend("darwin", Some(Backend::Launchd(b.clone())), fake.clone());
    assert_eq!(svc.unit_path(), Some(Location::Path(b.full.plist_path.clone())));
    assert_eq!(svc.log_path(), Some(Location::Path(home.join("Library/Logs/eve-bridge.log"))));
    let sd = Systemd { env: env_at(&home), unit_path: systemd::unit_path_under(&home), launch: b.launch.clone(), host: fake.clone() };
    let svc = Service::with_backend("linux", Some(Backend::Systemd(sd)), fake.clone());
    assert_eq!(svc.log_path().unwrap().to_string(), "journalctl --user -u eve-bridge.service");
    let ts = TaskScheduler { log_path: win::log_path_under(&home), launch: b.launch.clone(), host: fake.clone() };
    let svc = Service::with_backend("win32", Some(Backend::TaskScheduler(ts)), fake);
    assert_eq!(svc.unit_path(), Some(Location::Text("Task Scheduler: eve-bridge-daemon".into())));
    assert_eq!(svc.log_path(), Some(Location::Path(home.join("AppData/Local/eve-bridge/daemon.log"))));
}

// ==== full eve's launchd unit: install, uninstall, is_loaded ======================================================

#[test]
fn install_writes_full_eves_plist_and_loads_it() {
    let (_t, home) = home();
    let fake = Fake::new(&[], &[]);
    let b = launchd_in(&home, fake.clone());
    assert_eq!(b.install(Some("/dev/cu.usbmodem*")).unwrap(), 0);
    let path = home.join("Library/LaunchAgents/com.github.eve-bridge.daemon.plist");
    assert_eq!(std::fs::read(&path).unwrap(), b.build_plist(Some("/dev/cu.usbmodem*")).unwrap());
    assert!(home.join("Library/Logs").is_dir());
    let p = path.to_str().unwrap().to_string();
    assert_eq!(
        fake.calls(),
        [vec!["launchctl".to_string(), "unload".into(), p.clone()], vec!["launchctl".into(), "load".into(), "-w".into(), p]]
    );
    assert_eq!(
        fake.out(),
        format!(
            "installed: {}\nlogs at:   {}\ndaemon will start on your next login (and is starting now).\n",
            path.display(),
            home.join("Library/Logs/eve-bridge.log").display()
        )
    );
    assert!(b.is_installed());

    assert_eq!(b.uninstall().unwrap(), 0);
    assert!(!path.exists());
    assert_eq!(fake.out(), format!("removed: {}\n", path.display()));
    assert_eq!(b.uninstall().unwrap(), 0);
    assert_eq!(fake.out(), "service not installed; nothing to do\n");
}

#[test]
fn install_needs_launchctl_and_a_failed_load_is_an_exit_2() {
    let (_t, home) = home();
    let none = Fake::none();
    assert_eq!(launchd_in(&home, none.clone()).install(None).unwrap(), 2);
    assert_eq!(none.err(), "eve-bridge: `launchctl` not found on PATH\n");
    assert!(!home.join("Library").exists());

    let fake = Fake::new(&[], &["load"]);
    assert_eq!(launchd_in(&home, fake.clone()).install(None).unwrap(), 2);
    assert_eq!(fake.err(), "launchctl load failed (5): Load failed: 5: Input/output error\n");
    assert_eq!(fake.out(), "");
}

#[test]
fn widget_install_and_uninstall() {
    let (_t, home) = home();
    let fake = Fake::new(&[], &[]);
    let b = launchd_in(&home, fake.clone());
    assert_eq!(b.install_widget().unwrap(), 0);
    let path = home.join("Library/LaunchAgents/com.github.eve-bridge.notes-widget.plist");
    assert_eq!(std::fs::read(&path).unwrap(), b.build_widget_plist().unwrap());
    assert!(fake.out().ends_with("notes widget will show on your next login (and is showing now).\n"));
    assert_eq!(b.uninstall_widget().unwrap(), 0);
    assert!(!path.exists());
    assert_eq!(fake.verbs(), ["unload", "load", "unload"]);
    assert_eq!(b.uninstall_widget().unwrap(), 0);
    assert_eq!(fake.out(), format!("removed: {}\nnotes widget not installed; nothing to do\n", path.display()));
}

#[test]
fn is_loaded_finds_the_label_anywhere_in_a_line() {
    let (_t, home) = home();
    assert!(launchd_in(&home, Fake::new(&[LABEL], &[])).is_loaded().unwrap());
    assert!(!launchd_in(&home, Fake::new(&[LITE_LABEL], &[])).is_loaded().unwrap());
    assert!(!launchd_in(&home, Fake::none()).is_loaded().unwrap());
}

// ==== test_notes_widget.py: the launchd unit =======================================================================

#[test]
fn test_widget_plist_shape() {
    let (_t, home) = home();
    let b = launchd_in(&home, Fake::none());
    let parsed = parse(&b.build_widget_plist().unwrap());
    assert_eq!(s(&parsed, "Label"), "com.github.eve-bridge.notes-widget");
    assert_eq!(strings(&parsed, "ProgramArguments"), [PYTHON, "-m", "eve_bridge.cli", "notes-widget"]);
    assert_eq!(parsed.get("RunAtLoad").unwrap().as_boolean(), Some(true));
    assert_eq!(parsed.get("KeepAlive").unwrap().as_boolean(), Some(false));
    assert_eq!(s(&parsed, "ProcessType"), "Interactive");
    assert_eq!(s(&parsed, "StandardOutPath"), b.full.widget_log_path.to_str().unwrap());
    assert_eq!(s(&parsed, "StandardErrorPath"), b.full.widget_log_path.to_str().unwrap());
    let mut keys: Vec<String> = env_of(&parsed).into_keys().collect();
    keys.sort();
    assert_eq!(keys, ["HOME", "PATH"]);
}

#[test]
fn test_widget_plist_is_separate_from_daemon_plist() {
    let (_t, home) = home();
    let b = launchd_in(&home, Fake::none());
    let daemon = parse(&b.build_plist(None).unwrap());
    let widget = parse(&b.build_widget_plist().unwrap());
    assert_ne!(s(&daemon, "Label"), s(&widget, "Label"));
    assert_ne!(b.full.widget_plist_path, b.full.plist_path);
    assert_eq!(strings(&daemon, "ProgramArguments")[0], strings(&widget, "ProgramArguments")[0]);
    // The daemon unit keeps its shape — the widget is additive.
    assert_eq!(daemon.get("KeepAlive").unwrap().as_boolean(), Some(true));
    assert_eq!(strings(&daemon, "ProgramArguments").last().unwrap(), "daemon");
}

// ==== test_service_lite_plist.py: the plist ========================================================================

const LITE_KEYS: [&str; 10] = [
    "Label", "ProgramArguments", "RunAtLoad", "KeepAlive", "ThrottleInterval", "ProcessType", "Umask",
    "StandardOutPath", "StandardErrorPath", "EnvironmentVariables",
];

#[test]
fn test_the_lite_plist_holds_exactly_the_specs_keys_and_values() {
    let (_t, home) = home();
    let unit = lite(&launchd_in(&home, Fake::none()));
    let log = home.join("Library/Logs/eve-lite.log").to_str().unwrap().to_string();
    let mut keys: Vec<&str> = unit.keys().map(|k| k.as_str()).collect();
    keys.sort();
    let mut want = LITE_KEYS.to_vec();
    want.sort();
    assert_eq!(keys, want);
    assert_eq!(s(&unit, "Label"), "com.github.eve-bridge.lite");
    assert_eq!(LITE_LABEL, "com.github.eve-bridge.lite");
    assert_eq!(strings(&unit, "ProgramArguments"), [PYTHON, "-m", "eve_bridge.cli", "daemon"]);
    assert_eq!(unit.get("RunAtLoad").unwrap().as_boolean(), Some(true));
    // 75 (a sign-in) comes back; 0 (a stop) stays down
    let keep = unit.get("KeepAlive").unwrap().as_dictionary().unwrap();
    assert_eq!(keep.len(), 1);
    assert_eq!(keep.get("SuccessfulExit").unwrap().as_boolean(), Some(false));
    assert_eq!(unit.get("ThrottleInterval").unwrap().as_signed_integer(), Some(5));
    assert_eq!(s(&unit, "ProcessType"), "Interactive");
    assert_eq!(unit.get("Umask").unwrap().as_signed_integer(), Some(63));
    assert_eq!(0o077, 63);
    assert_eq!(s(&unit, "StandardOutPath"), log);
    assert_eq!(s(&unit, "StandardErrorPath"), log);
    let want: HashMap<String, String> = [
        ("HOME", home.to_str().unwrap()),
        ("PATH", "/usr/bin:/bin:/usr/sbin:/sbin"),
        ("EVE_PROFILE", "lite"),
        ("PYTHONNOUSERSITE", "1"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(env_of(&unit), want);
}

/// The plist pins the profile because the env file only fills gaps (envfile.py): a daemon started by it is eve lite
/// whatever the env file says.
#[test]
fn test_the_pinned_profile_is_the_one_profile_py_reads_as_lite() {
    let (_t, home) = home();
    let env = env_of(&lite(&launchd_in(&home, Fake::none())));
    assert!(eve_profile::is_lite(Some(&env)).unwrap());
    assert_eq!(eve_profile::active(Some(&env)).unwrap(), eve_profile::LITE);
    let control: HashMap<String, String> =
        env.into_iter().filter(|(k, _)| k != eve_profile::PROFILE_ENV).collect();
    assert!(!eve_profile::is_lite(Some(&control)).unwrap());
}

#[test]
fn test_the_lite_plist_follows_home_when_it_is_written() {
    let tmp = tempfile::tempdir().unwrap();
    let fake = Fake::none();
    // One backend; only its environment's HOME moves, as the Python test moves $HOME under one module.
    let mut b = launchd_in(&tmp.path().join("unused"), fake);
    for name in ["one", "two"] {
        let folder = tmp.path().join(name);
        std::fs::create_dir(&folder).unwrap();
        b.env.home = Some(folder.as_os_str().to_owned());
        let unit = lite(&b);
        assert_eq!(env_of(&unit)["HOME"], folder.to_str().unwrap());
        assert!(s(&unit, "StandardOutPath").starts_with(&format!("{}/", folder.display())));
        assert_eq!(b.lite_plist_path().unwrap(), folder.join("Library/LaunchAgents/com.github.eve-bridge.lite.plist"));
    }
}

#[test]
fn test_nothing_of_full_eves_unit_reaches_the_lite_plist() {
    let (tmp, home) = home();
    let mut b = launchd_in(&home, Fake::none());
    let before = b.build_lite_plist().unwrap();
    b.env.claude_config_dir = Some(OsString::from(tmp.path().join("wrapper-claude")));
    let after = b.build_lite_plist().unwrap();
    assert_eq!(after, before);
    let text = String::from_utf8(after).unwrap();
    for word in ["com.github.eve-bridge.daemon", "EVE_SERIAL_PORT", "EVE_CLAUDE_CONFIG_DIRS", "/opt/homebrew", "eve-bridge.log"] {
        assert!(!text.contains(word), "{word}");
    }
    // control: full eve's own plist, built in the same environment, carries both
    let full = String::from_utf8(b.build_plist(Some("/dev/cu.usbmodem*")).unwrap()).unwrap();
    assert!(full.contains("EVE_SERIAL_PORT") && full.contains("EVE_CLAUDE_CONFIG_DIRS"));
}

#[test]
fn test_plutil_reads_the_lite_plist() {
    if !Path::new("/usr/bin/plutil").exists() {
        return; // plutil is macOS's
    }
    let (tmp, home) = home();
    let path = tmp.path().join("lite.plist");
    std::fs::write(&path, launchd_in(&home, Fake::none()).build_lite_plist().unwrap()).unwrap();
    let done = std::process::Command::new("/usr/bin/plutil").arg("-lint").arg(&path).output().unwrap();
    let out = String::from_utf8_lossy(&done.stdout);
    assert!(done.status.success() && out.trim().ends_with("OK"), "{out}");
}

// ==== the owner golden: full eve's plists, byte for byte =========================================================

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/tests/golden/owner_main.json");
const SERIAL_GLOB: &str = "/dev/cu.usbmodem*";
const PLIST_HOME: &str = "/golden/home";
const PLIST_PYTHON: &str = "/golden/venv/bin/python3.12";
const PLIST_WRAPPER: &str = "/golden/wrapper-claude-home";

/// `owner_golden.plists()`: the units with HOME, the interpreter, the log paths and the Claude homes fixed.
fn golden_plists(label: &str) -> Vec<(&'static str, String)> {
    let home = Path::new(PLIST_HOME);
    let make = |claude: Option<&str>| Launchd {
        env: HomeEnv { claude_config_dir: claude.map(OsString::from), claude_config_dirs: None, home: Some(PLIST_HOME.into()) },
        full: FullPaths::under(home),
        label: label.to_string(),
        launch: Launch::Python(PLIST_PYTHON.into()),
        host: Fake::none(),
    };
    let text = |b: Vec<u8>| String::from_utf8(b).unwrap();
    vec![
        ("daemon_serial", text(make(None).build_plist(Some(SERIAL_GLOB)).unwrap())),
        ("daemon_plain", text(make(None).build_plist(None).unwrap())),
        ("daemon_wrapper_home", text(make(Some(PLIST_WRAPPER)).build_plist(Some(SERIAL_GLOB)).unwrap())),
        ("widget", text(make(None).build_widget_plist().unwrap())),
    ]
}

fn sha(text: &str) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(text.as_bytes()))
}

fn golden() -> serde_json::Value {
    let data = std::fs::read_to_string(GOLDEN).unwrap();
    let v: serde_json::Value = serde_json::from_str(&data).unwrap();
    v["snapshot"]["plist"].clone()
}

#[test]
fn test_full_eves_plists_are_byte_for_byte_the_golden() {
    let golden = golden();
    let now = golden_plists(LABEL);
    let mut names: Vec<&str> = golden.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    names.sort();
    assert_eq!(names, ["daemon_plain", "daemon_serial", "daemon_wrapper_home", "widget"]);
    assert_eq!(now.len(), 4);
    for (name, xml) in &now {
        assert_eq!(xml, golden[name]["xml"].as_str().unwrap(), "{name}");
        assert_eq!(sha(xml), golden[name]["sha256"].as_str().unwrap(), "{name}");
    }
}

/// Positive control for the test above: a changed label shows in every daemon unit, not in the widget's.
#[test]
fn test_a_full_plist_changed_in_code_does_not_match_the_golden() {
    let golden = golden();
    let now = golden_plists("com.example.another-daemon");
    let changed: Vec<&str> =
        now.iter().filter(|(n, xml)| xml != golden[n]["xml"].as_str().unwrap()).map(|(n, _)| *n).collect();
    assert_eq!(changed, ["daemon_serial", "daemon_plain", "daemon_wrapper_home"]);
}

// ==== test_service_lite_plist.py: install_lite and uninstall_lite ================================================

#[test]
fn test_install_lite_writes_the_plist_and_loads_it() {
    let (_t, home) = home();
    let fake = Fake::new(&[], &[]);
    let b = launchd_in(&home, fake.clone());
    assert_eq!(b.install_lite().unwrap(), 0);
    let path = home.join("Library/LaunchAgents/com.github.eve-bridge.lite.plist");
    assert_eq!(std::fs::read(&path).unwrap(), b.build_lite_plist().unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o644);
    }
    assert!(home.join("Library/Logs").is_dir());
    let p = path.to_str().unwrap().to_string();
    assert_eq!(
        fake.calls(),
        [vec!["launchctl".to_string(), "unload".into(), p.clone()], vec!["launchctl".into(), "load".into(), "-w".into(), p]]
    );
    // no temporary file left beside it
    let names: Vec<OsString> = std::fs::read_dir(path.parent().unwrap()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(names, [path.file_name().unwrap().to_owned()]);
    let out = fake.out();
    assert!(out.contains(&format!("installed: {}", path.display())));
    assert!(out.contains(home.join("Library/Logs/eve-lite.log").to_str().unwrap()));
}

/// An update runs `install --service` again: the plist then names the new program, and is reloaded.
#[test]
fn test_install_lite_again_replaces_the_plist_with_this_interpreter() {
    let (_t, home) = home();
    let fake = Fake::new(&[], &[]);
    let mut b = launchd_in(&home, fake.clone());
    assert_eq!(b.install_lite().unwrap(), 0);
    let python = home.join(".eve-lite/venv/bin/python").to_str().unwrap().to_string();
    b.launch = Launch::Python(python.clone());
    assert_eq!(b.install_lite().unwrap(), 0);
    assert_eq!(strings(&lite(&b), "ProgramArguments")[0], python);
    let unit = parse(&std::fs::read(b.lite_plist_path().unwrap()).unwrap());
    assert_eq!(strings(&unit, "ProgramArguments")[0], python);
    assert_eq!(fake.verbs(), ["unload", "load", "unload", "load"]);
}

#[test]
fn test_install_lite_refuses_beside_full_eves_agent() {
    let (_t, home) = home();
    let fake = Fake::new(&[], &[]);
    let b = launchd_in(&home, fake.clone());
    let full = home.join("Library/LaunchAgents/com.github.eve-bridge.daemon.plist");
    std::fs::create_dir_all(full.parent().unwrap()).unwrap();
    std::fs::write(&full, b"full eve's").unwrap();
    assert_eq!(b.install_lite().unwrap(), 2);
    assert!(fake.calls().is_empty());
    assert!(!b.lite_plist_path().unwrap().exists());
    assert_eq!(std::fs::read(&full).unwrap(), b"full eve's");
    let err = fake.err();
    assert!(err.contains("full eve's login service is installed"));
    assert_eq!(
        err,
        format!(
            "eve-bridge: full eve's login service is installed for this user ({}). Full eve and eve lite share \
             ~/.config/eve-bridge, so a Mac account has one of them. Nothing was changed.\n",
            full.display()
        )
    );
}

/// Control for the refusal: the agent looked for is under HOME now, never a path fixed at import (on the owner's
/// Mac, the module's own PLIST_PATH is full eve's real plist).
#[test]
fn test_install_lite_reads_full_eve_from_this_home_only() {
    let (tmp, home) = home();
    let fake = Fake::new(&[], &[]);
    let mut b = launchd_in(&home, fake);
    // full eve's import-time paths point elsewhere, where a plist exists
    let elsewhere = tmp.path().join("elsewhere");
    b.full = FullPaths::under(&elsewhere);
    std::fs::create_dir_all(b.full.plist_path.parent().unwrap()).unwrap();
    std::fs::write(&b.full.plist_path, b"<plist/>").unwrap();
    assert_eq!(b.agent_path(LABEL).unwrap().parent().unwrap(), home.join("Library/LaunchAgents"));
    assert_eq!(b.install_lite().unwrap(), 0);
}

#[test]
fn test_a_failed_load_is_an_exit_2() {
    let (_t, home) = home();
    let fake = Fake::new(&[], &["load"]);
    assert_eq!(launchd_in(&home, fake.clone()).install_lite().unwrap(), 2);
    assert!(fake.err().contains("launchctl load failed (5)"));
}

#[test]
fn test_install_lite_says_so_when_launchagents_cannot_be_written() {
    let (_t, home) = home();
    let fake = Fake::new(&[], &[]);
    std::fs::create_dir(home.join("Library")).unwrap();
    std::fs::write(home.join("Library/LaunchAgents"), "a file where the folder goes").unwrap();
    assert_eq!(launchd_in(&home, fake.clone()).install_lite().unwrap(), 2);
    assert!(fake.calls().is_empty());
    let err = fake.err();
    assert!(err.contains("eve lite's login service could not be written"));
    assert_eq!(
        err,
        format!(
            "eve-bridge: eve lite's login service could not be written to {} (FileExistsError).\n",
            home.join("Library/LaunchAgents").display()
        )
    );
}

/// The temporary file cannot be made in a LaunchAgents folder the user may not write: Python's mkstemp raises
/// PermissionError, and the message names it.
#[cfg(unix)]
#[test]
fn install_lite_names_the_exception_when_the_temporary_file_cannot_be_made() {
    use std::os::unix::fs::PermissionsExt;
    let (_t, home) = home();
    let agents = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o500)).unwrap();
    // root writes anyway; the case only exists for a user the folder refuses
    if std::fs::write(agents.join("probe"), b"").is_ok() {
        return;
    }
    let fake = Fake::new(&[], &[]);
    let rc = launchd_in(&home, fake.clone()).install_lite().unwrap();
    std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(rc, 2);
    assert!(fake.calls().is_empty());
    assert_eq!(
        fake.err(),
        format!("eve-bridge: eve lite's login service could not be written to {} (PermissionError).\n", agents.display())
    );
}

#[test]
fn test_install_lite_needs_launchctl() {
    let (_t, home) = home();
    let fake = Fake::none();
    let b = launchd_in(&home, fake.clone());
    assert_eq!(b.install_lite().unwrap(), 2);
    assert!(!b.lite_plist_path().unwrap().exists());
    assert!(fake.calls().is_empty());
}

fn write_lite(b: &Launchd) -> PathBuf {
    let path = b.lite_plist_path().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b.build_lite_plist().unwrap()).unwrap();
    path
}

#[test]
fn test_uninstall_lite_stops_eve_lite_by_its_label_and_removes_the_plist() {
    let (_t, home) = home();
    let fake = Fake::new(&[LITE_LABEL], &[]);
    let b = launchd_in(&home, fake.clone());
    let path = write_lite(&b);
    assert_eq!(b.uninstall_lite().unwrap(), 0);
    assert!(!path.exists());
    assert_eq!(
        fake.calls()[1..],
        [vec!["launchctl".to_string(), "bootout".into(), format!("gui/{UID}/com.github.eve-bridge.lite")]]
    );
    assert!(!fake.calls().iter().any(|c| c.join(" ").contains("com.github.eve-bridge.daemon")));
    assert_eq!(fake.out(), format!("stopped eve lite\nremoved: {}\n", path.display()));
}

#[test]
fn test_uninstall_lite_unloads_by_file_when_bootout_fails() {
    let (_t, home) = home();
    let fake = Fake::new(&[LITE_LABEL], &["bootout"]);
    let b = launchd_in(&home, fake.clone());
    let path = write_lite(&b);
    assert_eq!(b.uninstall_lite().unwrap(), 0);
    assert_eq!(fake.verbs(), ["list", "bootout", "unload"]);
    assert!(!path.exists());
}

#[test]
fn test_uninstall_lite_stops_a_loaded_agent_whose_file_is_gone() {
    let (_t, home) = home();
    let fake = Fake::new(&[LITE_LABEL], &[]);
    assert_eq!(launchd_in(&home, fake.clone()).uninstall_lite().unwrap(), 0);
    assert_eq!(fake.verbs(), ["list", "bootout"]);
    assert_eq!(fake.out(), "stopped eve lite\n");
}

#[test]
fn test_uninstall_lite_with_nothing_installed_does_nothing() {
    let (_t, home) = home();
    let fake = Fake::new(&[], &[]);
    assert_eq!(launchd_in(&home, fake.clone()).uninstall_lite().unwrap(), 0);
    assert_eq!(fake.verbs(), ["list"]);
    assert!(fake.out().contains("not installed; nothing to do"));
}

#[test]
fn test_lite_loaded_matches_the_label_whole() {
    let (_t, home) = home();
    let cases: [(&[&str], bool); 4] = [
        (&["com.github.eve-bridge.lite"], true),
        (&["com.github.eve-bridge.daemon"], false), // full eve's agent is not eve lite's
        (&["com.github.eve-bridge.lite.helper", "x.com.github.eve-bridge.lite"], false),
        (&[], false),
    ];
    for (listed, loaded) in cases {
        assert_eq!(launchd_in(&home, Fake::new(listed, &[])).lite_loaded().unwrap(), loaded, "{listed:?}");
    }
}

#[test]
fn test_installed_lite_plist_reads_only_a_real_plist() {
    let (_t, home) = home();
    let b = launchd_in(&home, Fake::none());
    assert_eq!(b.installed_lite_plist(), None);
    let path = b.lite_plist_path().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"not a plist").unwrap();
    assert_eq!(b.installed_lite_plist(), None);
    std::fs::write(&path, b.build_lite_plist().unwrap()).unwrap();
    assert_eq!(b.installed_lite_plist(), Some(lite(&b)));
    assert!(b.lite_installed().unwrap());
}

#[test]
fn installed_lite_plist_is_none_for_a_plist_that_is_not_a_dict() {
    let (_t, home) = home();
    let b = launchd_in(&home, Fake::none());
    let path = b.lite_plist_path().unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, crate::plistxml::dumps(&crate::plistxml::Value::Array(vec![])).unwrap()).unwrap();
    assert_eq!(b.installed_lite_plist(), None);
}

// ==== test_service_systemd.py =====================================================================================

/// Tiny INI-ish parser: section name -> {key: value}.
fn parse_unit(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut sections: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current: Option<String> = None;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let name = line[1..line.len() - 1].to_string();
            sections.entry(name.clone()).or_default();
            current = Some(name);
            continue;
        }
        let section = current.as_ref().unwrap_or_else(|| panic!("key before any [Section]: {raw:?}"));
        let (key, value) = line.split_once('=').unwrap_or((line, ""));
        sections.get_mut(section).unwrap().insert(key.trim().to_string(), value.trim().to_string());
    }
    sections
}

fn systemd_in(home: &Path, host: Arc<Fake>) -> Systemd {
    Systemd { env: env_at(home), unit_path: systemd::unit_path_under(home), launch: Launch::Python(PYTHON.into()), host }
}

fn unit(home: &Path) -> HashMap<String, HashMap<String, String>> {
    parse_unit(&systemd_in(home, Fake::none()).build_unit(None).unwrap())
}

#[test]
fn test_unit_has_required_sections() {
    let (_t, home) = home();
    let sections = unit(&home);
    for name in ["Unit", "Service", "Install"] {
        assert!(sections.contains_key(name));
    }
}

#[test]
fn test_unit_description_is_set() {
    let (_t, home) = home();
    assert!(!unit(&home)["Unit"]["Description"].is_empty());
}

#[test]
fn test_unit_execstart_uses_current_interpreter() {
    let (_t, home) = home();
    // Must invoke the same program that's running the install.
    assert_eq!(unit(&home)["Service"]["ExecStart"], format!("{PYTHON} -m eve_bridge.cli daemon"));
}

#[test]
fn test_unit_restarts_on_failure() {
    let (_t, home) = home();
    let sections = unit(&home);
    assert_eq!(sections["Service"]["Type"], "simple");
    assert_eq!(sections["Service"]["Restart"], "on-failure");
    // Some delay so we don't hot-loop if BLE setup is permanently broken.
    assert!(sections["Service"]["RestartSec"].parse::<i64>().unwrap() >= 1);
}

#[test]
fn test_unit_install_target_is_default() {
    let (_t, home) = home();
    // default.target is correct for user units — graphical-session.target only exists in a graphical session,
    // which would gate out terminal-only users.
    assert_eq!(unit(&home)["Install"]["WantedBy"], "default.target");
}

#[test]
fn test_unit_path_under_user_systemd_dir() {
    let (_t, home) = home();
    let p = systemd::unit_path_under(&home);
    assert_eq!(p.file_name().unwrap(), "eve-bridge.service");
    let parts: Vec<_> = p.parent().unwrap().components().rev().take(3).map(|c| c.as_os_str().to_owned()).collect();
    assert_eq!(parts, [OsString::from("user"), "systemd".into(), ".config".into()]);
}

#[test]
fn test_log_path_points_at_journalctl() {
    let (_t, home) = home();
    let log_target = systemd_in(&home, Fake::none()).log_path();
    assert!(log_target.contains("journalctl"));
    assert!(log_target.contains(UNIT_NAME));
}

#[test]
fn the_unit_is_byte_for_byte_pythons() {
    let (tmp, home) = home();
    let mut sd = systemd_in(&home, Fake::none());
    assert_eq!(
        sd.build_unit(Some("/dev/ttyACM0")).unwrap(),
        format!(
            "[Unit]\nDescription=Claude Code <-> desktop-eve BLE bridge\nAfter=default.target\n\n[Service]\n\
             Type=simple\nEnvironment=\"EVE_SERIAL_PORT=/dev/ttyACM0\"\nExecStart={PYTHON} -m eve_bridge.cli daemon\n\
             Restart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n"
        )
    );
    let wrapper = tmp.path().join("wrapper");
    sd.env.claude_config_dir = Some(wrapper.as_os_str().to_owned());
    let text = sd.build_unit(None).unwrap();
    assert!(text.contains(&format!(
        "Type=simple\nEnvironment=\"EVE_CLAUDE_CONFIG_DIRS={}:{}\"\nExecStart=",
        wrapper.display(),
        home.join(".claude").display()
    )));
}

#[test]
fn systemd_install_reloads_and_enables_then_uninstall_disables() {
    let (_t, home) = home();
    let fake = Fake::new(&[], &[]);
    let sd = systemd_in(&home, fake.clone());
    assert_eq!(sd.install(None).unwrap(), 0);
    assert_eq!(std::fs::read_to_string(&sd.unit_path).unwrap(), sd.build_unit(None).unwrap());
    let argv = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    assert_eq!(
        fake.calls(),
        [argv(&["systemctl", "--user", "daemon-reload"]), argv(&["systemctl", "--user", "enable", "--now", UNIT_NAME])]
    );
    assert!(fake.out().starts_with(&format!("installed: {}\nlogs:      journalctl --user -u eve-bridge.service\n", sd.unit_path.display())));
    assert!(sd.is_installed());
    assert!(sd.is_loaded().unwrap());
    assert_eq!(sd.uninstall().unwrap(), 0);
    assert!(!sd.unit_path.exists());
    assert_eq!(fake.calls().len(), 5); // daemon-reload, enable, is-active, disable, daemon-reload
    assert_eq!(fake.calls()[3], argv(&["systemctl", "--user", "disable", "--now", UNIT_NAME]));
    assert_eq!(fake.calls()[4], argv(&["systemctl", "--user", "daemon-reload"]));
}

#[test]
fn systemd_install_reports_a_failed_reload() {
    let (_t, home) = home();
    let fake = Fake::new(&[], &["--user"]);
    assert_eq!(systemd_in(&home, fake.clone()).install(None).unwrap(), 2);
    assert_eq!(fake.err(), "systemctl --user daemon-reload failed (5): Load failed: 5: Input/output error\n");
    let none = Fake::none();
    assert_eq!(systemd_in(&home, none.clone()).install(None).unwrap(), 2);
    assert_eq!(none.err(), "eve-bridge: `systemctl` not found on PATH\n");
}

// ==== test_service_windows.py =====================================================================================

/// The schtasks fixture: a log folder with a space in it, an interpreter under Program Files.
fn schtasks(tmp: &Path) -> (TaskScheduler, Arc<Fake>) {
    let fake = Fake::new(&[], &[]);
    let ts = TaskScheduler {
        log_path: tmp.join("eve bridge").join("daemon.log"),
        launch: Launch::Python("C:\\Program Files\\Python 3.12\\python.exe".into()),
        host: fake.clone(),
    };
    (ts, fake)
}

fn tr(calls: &[Vec<String>]) -> String {
    let args = calls.last().unwrap();
    let i = args.iter().position(|a| a == "/tr").unwrap();
    args[i + 1].clone()
}

/// /tr is run as a program, not through cmd.exe, so `set ... &&` and `>> log 2>&1` never worked: the task runs a
/// launcher script with cmd /c instead, and the script holds the cmd syntax.
#[test]
fn test_the_task_runs_its_redirect_and_env_through_cmd() {
    let tmp = tempfile::tempdir().unwrap();
    let (win, fake) = schtasks(tmp.path());
    assert_eq!(win.install(Some("COM3")).unwrap(), 0);
    let tr = tr(&fake.calls());
    let launcher = win.log_path.parent().unwrap().join("daemon.cmd");
    assert_eq!(tr, format!("cmd.exe /c \"{}\"", launcher.display()));
    assert!(tr.len() <= 261); // schtasks' /tr limit
    let script = std::fs::read_to_string(&launcher).unwrap();
    assert!(script.contains("set \"EVE_SERIAL_PORT=COM3\""));
    assert!(script.contains(&format!(
        "\"C:\\Program Files\\Python 3.12\\python.exe\" -m eve_bridge.cli daemon >> \"{}\" 2>&1",
        win.log_path.display()
    )));
    assert!(!tr.contains("&&") && !tr.contains(">>"));
    assert_eq!(
        fake.calls()[0],
        ["schtasks", "/create", "/tn", "eve-bridge-daemon", "/tr", &tr, "/sc", "onlogon", "/f"]
    );
    assert!(script.starts_with("@echo off\r\n") && script.ends_with(" 2>&1\r\n"));
}

#[test]
fn test_without_a_port_the_launcher_sets_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let (win, _fake) = schtasks(tmp.path());
    assert_eq!(win.install(None).unwrap(), 0);
    assert!(!std::fs::read_to_string(win.launcher_path()).unwrap().contains("EVE_SERIAL_PORT"));
}

#[test]
fn test_a_percent_sign_is_escaped_for_the_batch_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (mut win, _fake) = schtasks(tmp.path());
    win.launch = Launch::Python("C:\\100%\\python.exe".into());
    assert_eq!(win.install(None).unwrap(), 0);
    assert!(std::fs::read_to_string(win.launcher_path()).unwrap().contains("\"C:\\100%%\\python.exe\""));
}

#[test]
fn test_uninstall_removes_the_launcher() {
    let tmp = tempfile::tempdir().unwrap();
    let (win, fake) = schtasks(tmp.path());
    assert_eq!(win.install(None).unwrap(), 0);
    assert_eq!(win.uninstall().unwrap(), 0);
    assert!(!win.launcher_path().exists());
    assert_eq!(fake.verbs(), ["/create", "/query", "/delete"]);
    assert!(fake.out().ends_with("removed: Task Scheduler task 'eve-bridge-daemon'\n"));
}

#[test]
fn windows_install_needs_schtasks_and_reports_a_failed_create() {
    let tmp = tempfile::tempdir().unwrap();
    let none = Fake::none();
    let mut win = TaskScheduler {
        log_path: tmp.path().join("l").join("daemon.log"),
        launch: Launch::Python("py".into()),
        host: none.clone(),
    };
    assert_eq!(win.install(None).unwrap(), 2);
    assert_eq!(none.err(), "eve-bridge: `schtasks` not found on PATH\n");
    assert!(!win.is_installed().unwrap());
    assert_eq!(win.uninstall().unwrap(), 0);
    assert_eq!(none.out(), "service not installed; nothing to do\n");
    let failing = Fake::new(&[], &["/create"]);
    win.host = failing.clone();
    assert_eq!(win.install(None).unwrap(), 2);
    assert_eq!(failing.err(), "schtasks create failed (5): Load failed: 5: Input/output error\n");
    assert_eq!(launchd::NAME, "launchd");
}

// ==== cross-check against the Python modules themselves, when a python3 can import them ===========================

const BRIDGE_SRC: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src");

/// The Python modules' own output for HOME=`home`, CLAUDE_CONFIG_DIR=`claude` and sys.executable=`/py`, or None when
/// no python3 here can import them.
fn python_units(home: &Path, claude: &Path) -> Option<String> {
    let python = which::which("python3").ok()?;
    let script = format!(
        "import sys; sys.path.insert(0, {src:?}); sys.executable = '/py'\n\
         from eve_bridge import _service_launchd as l, _service_systemd as s\n\
         out = sys.stdout.buffer\n\
         for b in (l._build_lite_plist(), l._build_plist('/dev/cu.x&<y>'), l._build_widget_plist()):\n\
         \x20   out.write(b); out.write(b'\\0')\n\
         out.write(s._build_unit('/dev/ttyACM0').encode())\n",
        src = BRIDGE_SRC
    );
    let done = std::process::Command::new(python)
        .arg("-I")
        .arg("-c")
        .arg(script)
        .env_clear()
        .env("HOME", home)
        .env("CLAUDE_CONFIG_DIR", claude)
        .output()
        .ok()?;
    done.status.success().then(|| String::from_utf8(done.stdout).unwrap())
}

#[test]
fn every_unit_matches_the_python_modules_byte_for_byte() {
    let (tmp, home) = home();
    let claude = tmp.path().join("wrapper");
    let Some(want) = python_units(&home, &claude) else { return };
    eprintln!("compared with python: {} bytes", want.len());
    let b = Launchd {
        env: HomeEnv { claude_config_dir: Some(claude.as_os_str().to_owned()), claude_config_dirs: None, home: Some(home.as_os_str().to_owned()) },
        full: FullPaths::under(&home),
        label: LABEL.into(),
        launch: Launch::Python("/py".into()),
        host: Fake::none(),
    };
    let sd = Systemd { env: b.env.clone(), unit_path: systemd::unit_path_under(&home), launch: b.launch.clone(), host: Fake::none() };
    let mut got = String::new();
    for bytes in [b.build_lite_plist().unwrap(), b.build_plist(Some("/dev/cu.x&<y>")).unwrap(), b.build_widget_plist().unwrap()] {
        got.push_str(&String::from_utf8(bytes).unwrap());
        got.push('\0');
    }
    got.push_str(&sd.build_unit(Some("/dev/ttyACM0")).unwrap());
    assert_eq!(got, want);
}
