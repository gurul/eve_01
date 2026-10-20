//! Ports of tests/test_cli_lite.py and tests/test_cli_memory.py, and of the CLI parts of test_diag_render,
//! test_mic, test_daemon_lesson, test_link_pair_cli, test_notify, test_chat_memory and test_learning_search.
//! Every daemon here is a stand-in on a real Unix socket in a short temporary folder; launchctl, osadecompile and
//! open never run (recorders stand in for them), and HOME is a temporary folder throughout.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::lite::{self, Ctx};
use super::memory::{self, NightDreamer, Seams, Store, StoreIndex};
use super::*;
use crate::io::{capture, Captured};

const CANARY: &str = "CANARY-NOT-A-KEY-7f3c"; // a one-time code and a key that are not token-shaped
const PORT: i64 = 48780;
const FULL_LABEL: &str = "com.github.eve-bridge.daemon";
const LITE_LABEL: &str = "com.github.eve-bridge.lite";

fn s(words: &[&str]) -> Vec<String> {
    words.iter().map(|w| w.to_string()).collect()
}

fn no_post() -> crate::io::Post {
    Box::new(|_, _, _, _| None)
}

fn no_env(_: &str) -> Option<String> {
    None
}

// ---- the stand-ins --------------------------------------------------------------------------------------------

/// `handler` answering on a real IPC socket (one JSON line in, one out), on a thread of its own.
struct Serving {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Serving {
    fn new(path: &Path, handler: Arc<dyn Fn(&Value) -> Value + Send + Sync>) -> Serving {
        let _ = std::fs::remove_file(path); // the last stand-in's socket file
        let listener = UnixListener::bind(path).expect("bind the stand-in socket");
        listener.set_nonblocking(true).expect("nonblocking");
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::spawn(move || {
            while !flag.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => {
                        let _ = stream.set_nonblocking(false);
                        let mut reader = BufReader::new(stream.try_clone().expect("clone"));
                        let mut line = String::new();
                        if reader.read_line(&mut line).is_ok() {
                            if let Ok(req) = serde_json::from_str::<Value>(&line) {
                                let answer = handler(&req);
                                let mut w = stream;
                                let _ = w.write_all(format!("{answer}\n").as_bytes());
                            }
                        }
                    }
                    Err(_) => std::thread::sleep(Duration::from_millis(2)),
                }
            }
        });
        Serving { stop, thread: Some(thread) }
    }
}

impl Drop for Serving {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// lite.py's IPC as the CLI sees it: `web_login` hands out `code` for `port` (after `page_down` answers that the
/// page is not up yet), `web_login_used` says whether the browser used it, and other events answer what the test
/// gave in `answers`. Every request is kept in `seen`.
struct Daemon {
    code: Value,
    port: Value,
    page_down: AtomicI64,
    answers: Mutex<HashMap<String, Value>>,
    used: AtomicBool,
    seen: Mutex<Vec<Value>>,
}

impl Daemon {
    fn new() -> Arc<Daemon> {
        Daemon::with(json!(CANARY), json!(PORT), 0, &[])
    }

    fn with(code: Value, port: Value, page_down: i64, answers: &[(&str, Value)]) -> Arc<Daemon> {
        Arc::new(Daemon {
            code,
            port,
            page_down: AtomicI64::new(page_down),
            answers: Mutex::new(answers.iter().map(|(k, v)| (k.to_string(), v.clone())).collect()),
            used: AtomicBool::new(false),
            seen: Mutex::new(Vec::new()),
        })
    }

    fn answering(answers: &[(&str, Value)]) -> Arc<Daemon> {
        Daemon::with(json!(CANARY), json!(PORT), 0, answers)
    }

    fn answer(&self, req: &Value) -> Value {
        self.seen.lock().unwrap().push(req.clone());
        match req.get("evt").and_then(Value::as_str).unwrap_or("") {
            "web_login" => {
                if self.page_down.load(Ordering::SeqCst) > 0 {
                    self.page_down.fetch_sub(1, Ordering::SeqCst);
                    return json!({"ok": false, "error": eve_lite::PAGE_DOWN});
                }
                json!({"ok": true, "code": self.code, "port": self.port})
            }
            "web_login_used" => {
                json!({"ok": true, "used": self.used.load(Ordering::SeqCst) && req.get("code") == Some(&self.code)})
            }
            evt => match self.answers.lock().unwrap().get(evt) {
                Some(a) => a.clone(),
                None => json!({"ok": false, "error": eve_lite::UNKNOWN_EVENT}),
            },
        }
    }

    fn events(&self) -> Vec<String> {
        self.seen.lock().unwrap().iter().map(|r| r.get("evt").and_then(Value::as_str).unwrap_or("").to_string()).collect()
    }

    fn serve(self: &Arc<Self>, path: &Path) -> Serving {
        let me = self.clone();
        Serving::new(path, Arc::new(move |req| me.answer(req)))
    }
}

/// _open_in_browser's stand-in: reads the page as a browser would, notes its mode and folder, and signs in with
/// the code its address carries (`sign_in`), as the page's POST to /api/mac/login does.
#[derive(Default)]
struct Browser {
    paths: Mutex<Vec<String>>,
    modes: Mutex<Vec<u32>>,
    pages: Mutex<Vec<String>>,
    codes: Mutex<Vec<String>>,
}

impl Browser {
    fn open(&self, path: &str, opens: bool, sign_in: Option<&Arc<Daemon>>) -> bool {
        self.paths.lock().unwrap().push(path.to_string());
        self.modes.lock().unwrap().push(std::fs::metadata(path).unwrap().permissions().mode() & 0o7777);
        let page = std::fs::read_to_string(path).unwrap();
        let start = page.find("location.replace(").unwrap() + "location.replace(".len();
        let end = page[start..].find(')').unwrap() + start;
        let url: String = serde_json::from_str(&page[start..end]).unwrap();
        let code = url.split_once("#login=").unwrap().1.to_string();
        self.pages.lock().unwrap().push(page);
        self.codes.lock().unwrap().push(code.clone());
        if opens {
            if let Some(d) = sign_in {
                d.used.store(serde_json::Value::String(code) == d.code, Ordering::SeqCst);
            }
        }
        opens
    }
}

/// The process boundary of launchd: launchctl is found only when `launchctl` is set; `list` shows `listed`.
#[derive(Default)]
struct Host {
    launchctl: bool,
    listed: Vec<String>,
    calls: Mutex<Vec<Vec<String>>>,
    printed: Mutex<Vec<String>>,
}

impl eve_service::Host for Host {
    fn which(&self, name: &str) -> Option<PathBuf> {
        (self.launchctl && name == "launchctl").then(|| PathBuf::from("/bin/launchctl"))
    }
    fn run(&self, argv: &[String]) -> std::io::Result<eve_service::Completed> {
        self.calls.lock().unwrap().push(argv.to_vec());
        assert_eq!(argv[0], "launchctl", "{} would have run", argv[0]);
        if argv.get(1).map(String::as_str) == Some("list") {
            let rows: String = self.listed.iter().map(|l| format!("-\t0\t{l}\n")).collect();
            return Ok(eve_service::Completed::new(0, format!("PID\tStatus\tLabel\n{rows}"), ""));
        }
        Ok(eve_service::Completed::new(0, "", ""))
    }
    fn print(&self, line: &str) {
        self.printed.lock().unwrap().push(line.to_string());
    }
    fn eprint(&self, line: &str) {
        self.printed.lock().unwrap().push(line.to_string());
    }
    fn getuid(&self) -> std::io::Result<u32> {
        // SAFETY: getuid cannot fail.
        Ok(unsafe { libc::getuid() })
    }
}

/// subprocess.run for the programs eve lite's CLI starts besides launchctl: osadecompile (`script` is what an
/// Eve.app holds) and open. Each call is recorded; nothing runs.
#[derive(Default)]
struct Programs {
    calls: Mutex<Vec<Vec<String>>>,
    script: Mutex<String>,
    fail: Mutex<Vec<String>>,
}

impl Programs {
    fn run(&self, argv: &[String]) -> std::io::Result<(i32, String)> {
        self.calls.lock().unwrap().push(argv.to_vec());
        let program = Path::new(&argv[0]).file_name().unwrap().to_string_lossy().into_owned();
        assert!(program == "osadecompile" || program == "open", "{program} would have run");
        let key = if program == "open" { argv[..argv.len().min(3)].join(" ") } else { program.clone() };
        if self.fail.lock().unwrap().iter().any(|f| key.starts_with(f.as_str())) {
            return Ok((1, String::new()));
        }
        if program == "osadecompile" {
            let script = self.script.lock().unwrap().clone();
            return Ok((if script.is_empty() { 1 } else { 0 }, script));
        }
        Ok((0, String::new()))
    }
}

/// eve lite's world for one test: HOME, its scratch TMPDIR and the socket's short folder.
struct World {
    _tmp: tempfile::TempDir,
    _sockdir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
    sock: PathBuf,
    env: HashMap<String, String>,
    host: Arc<Host>,
    programs: Arc<Programs>,
}

impl World {
    fn new() -> World {
        World::with_host(Host::default())
    }

    fn with_host(host: Host) -> World {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".config/eve-bridge")).unwrap();
        std::fs::create_dir_all(root.join("t")).unwrap();
        // macOS keeps a socket's path in 104 bytes
        let sockdir = tempfile::Builder::new().prefix("evc-").tempdir_in("/tmp").unwrap();
        let sock = sockdir.path().join("ipc.sock");
        let mut env: HashMap<String, String> = [
            ("HOME".to_string(), home.to_string_lossy().into_owned()),
            ("EVE_PROFILE".to_string(), "lite".to_string()),
            ("TMPDIR".to_string(), root.join("t").to_string_lossy().into_owned()),
            ("EVE_BRIDGE_SOCK".to_string(), sock.to_string_lossy().into_owned()),
        ]
        .into();
        eve_profile::apply(&mut env).unwrap();
        World { _tmp: tmp, _sockdir: sockdir, root, home, sock, env, host: Arc::new(host), programs: Arc::default() }
    }

    fn launchctl() -> Host {
        Host { launchctl: true, listed: vec![LITE_LABEL.to_string()], ..Host::default() }
    }

    fn ctx(&self) -> Ctx {
        let programs = self.programs.clone();
        Ctx {
            env: self.env.clone(),
            home: self.home.clone(),
            launchd: lite::launchd_for(&self.home, self.host.clone()),
            platform: "darwin".into(),
            run: Box::new(move |argv, _| programs.run(argv)),
            open_in_browser: Some(Box::new(|_| panic!("a page was opened"))),
            poll_secs: 0.01,
            login_secs: eve_web_auth::LOGIN_SECS,
            run_daemon: Box::new(|_| panic!("eve lite's daemon ran")),
            setup_logging: Box::new(|_| {}),
            memory: Seams {
                open: Box::new(|_| panic!("the memory store was opened")),
                make_client: Box::new(|| None),
                dreamer: memory::real_dreamer(),
            },
            catch_signals: false,
        }
    }

    fn pages(&self) -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(self.root.join("t")).unwrap().map(|e| e.unwrap().path()).collect();
        v.sort();
        v
    }

    fn launchd_calls(&self) -> Vec<Vec<String>> {
        self.host.calls.lock().unwrap().clone()
    }
}

fn run_lite(argv: &[&str], ctx: &Ctx) -> (i32, Captured) {
    let (io, seen) = capture(Box::new(crate::io::Io::process().post), None);
    let rc = lite::lite_main(&s(argv), &[], ctx, &io);
    (rc, Captured::take(&seen))
}

fn run_full(argv: &[&str]) -> (i32, Captured) {
    let (io, seen) = capture(no_post(), None);
    let rc = full_main(&s(argv), &io, &no_env);
    (rc, Captured::take(&seen))
}

// ---- the profile, and which command line runs ----------------------------------------------------------------

#[test]
fn test_full_eves_command_line_has_none_of_eve_lites_commands() {
    for words in [&["web", "open"][..], &["doctor"], &["lite", "uninstall"]] {
        let (rc, out) = run_full(words);
        assert_eq!(rc, 2);
        assert!(out.err.contains(&format!("invalid choice: '{}'", words[0])), "{}", out.err);
    }
}

#[test]
fn test_eve_lites_command_line_has_none_of_full_eves_others() {
    let w = World::new();
    let ctx = w.ctx();
    for command in [
        "celebrate", "species", "diag", "hud", "unpair", "key-check", "ears-check", "move", "head-cal", "link",
        "notify", "apps", "telegram-check", "check-update", "audit", "learning", "lights", "codex-home",
        "chrome-profile", "vault", "app",
    ] {
        let (rc, out) = run_lite(&[command], &ctx);
        assert_eq!(rc, 2, "{command}");
        assert!(out.err.contains(&format!("invalid choice: '{command}'")), "{}", out.err);
    }
}

#[test]
fn test_eve_lites_commands_are_its_own() {
    assert_eq!(
        lite::lite_parser().command_names(),
        s(&["daemon", "install", "uninstall", "status", "update", "web", "doctor", "lite", "memory"])
    );
    let w = World::new();
    let (rc, out) = run_lite(&[], &w.ctx());
    assert_eq!(rc, 1);
    assert!(out.out.contains("eve lite on this Mac"));
}

#[test]
fn test_web_open_takes_the_seconds_the_installer_gives_it() {
    let parser = lite::lite_parser();
    let mut sink = Vec::new();
    let mut errs = Vec::new();
    let ns = parser.parse_args_to(&s(&["web", "open", "--wait", "90"]), &mut sink, &mut errs).unwrap();
    assert_eq!(ns.get("wait"), &Val::Float(90.0));
    let ns = parser.parse_args_to(&s(&["web", "open"]), &mut sink, &mut errs).unwrap();
    assert_eq!(ns.get("wait"), &Val::Float(lite::WEB_OPEN_WAIT_SECS));
    for bad in ["-1", "nan", "inf", "soon"] {
        let mut errs = Vec::new();
        assert!(parser.parse_args_to(&s(&["web", "open", "--wait", bad]), &mut sink, &mut errs).is_err(), "{bad}");
    }
    let mut errs = Vec::new();
    let _ = parser.parse_args_to(&s(&["web", "open", "--wait", "soon"]), &mut sink, &mut errs);
    assert!(String::from_utf8_lossy(&errs).contains("argument --wait: not a number of seconds: 'soon'"));
}

// ---- the daemon ----------------------------------------------------------------------------------------------

#[test]
fn test_daemon_under_full_eve_still_builds_full_eves_daemon() {
    let parser = full_parser(&no_env);
    let mut sink = Vec::new();
    let ns = parser.parse_args_to(&s(&["daemon", "--socket", "/tmp/x.sock"]), &mut sink, &mut Vec::new()).unwrap();
    let o = daemon_options(&ns);
    assert_eq!(o.socket_path.as_deref(), Some("/tmp/x.sock"));
    assert_eq!(o.device_name_prefix, "Claude");
    assert_eq!((o.device_address, o.serial_port, o.save_frames), (None, None, None));
    // EVE_SERIAL_PORT and EVE_SAVE_FRAMES are the defaults when set
    let env = |name: &str| match name {
        "EVE_SERIAL_PORT" => Some("/dev/cu.x".to_string()),
        "EVE_SAVE_FRAMES" => Some("/tmp/frames".to_string()),
        _ => None,
    };
    let ns = full_parser(&env).parse_args_to(&s(&["daemon"]), &mut sink, &mut Vec::new()).unwrap();
    let o = daemon_options(&ns);
    assert_eq!(o.serial_port.as_deref(), Some("/dev/cu.x"));
    assert_eq!(o.save_frames.as_deref(), Some("/tmp/frames"));
}

#[test]
fn test_daemon_under_eve_lite_runs_lite_daemon_on_lite_switches() {
    let mut env: HashMap<String, String> = [
        ("EVE_PROFILE".to_string(), "lite".to_string()),
        ("EVE_TELEGRAM_TOKEN".to_string(), format!("{CANARY}-tg")),
        ("OPENROUTER_API_KEY".to_string(), format!("{CANARY}-or")),
    ]
    .into();
    let applied = eve_profile::apply(&mut env).unwrap();
    assert_eq!(env.get("EVE_TELEGRAM").map(String::as_str), Some("app")); // forced
    assert_eq!(env.get("EVE_MINIAPP").map(String::as_str), Some("0"));
    assert!(!env.contains_key("EVE_TELEGRAM_TOKEN") && env["OPENROUTER_API_KEY"] == format!("{CANARY}-or"));

    let w = World::new();
    let mut ctx = w.ctx();
    let seen: Arc<Mutex<Vec<eve_lite::DaemonArgs>>> = Arc::default();
    let level: Arc<Mutex<String>> = Arc::default();
    let (s1, l1) = (seen.clone(), level.clone());
    ctx.run_daemon = Box::new(move |args| {
        s1.lock().unwrap().push(args.clone());
        eve_lite::EXIT_RESTART
    });
    ctx.setup_logging = Box::new(move |l| *l1.lock().unwrap() = l.to_string());
    let (io, _) = capture(no_post(), None);
    assert_eq!(lite::lite_main(&s(&["daemon", "--log-level", "DEBUG"]), &applied, &ctx, &io), 75);
    let args = seen.lock().unwrap()[0].clone();
    assert_eq!(args.socket, None);
    assert_eq!(args.log_level.as_deref(), Some("DEBUG"));
    assert_eq!(*level.lock().unwrap(), "DEBUG");
    let said = lite::profile_line(&applied);
    assert!(said.starts_with("profile: eve lite:"));
    let removed: Vec<&str> = said.split_once("removed ").unwrap().1.split(", ").collect();
    assert!(removed.contains(&"EVE_TELEGRAM_TOKEN"));
    assert!(!said.contains("OPENROUTER_API_KEY") && !said.contains(CANARY));
}

// ---- web open ------------------------------------------------------------------------------------------------

fn browser_opens(ctx: &mut Ctx, browser: &Arc<Browser>, opens: bool, sign_in: Option<Arc<Daemon>>) {
    let b = browser.clone();
    ctx.open_in_browser = Some(Box::new(move |path| b.open(path, opens, sign_in.as_ref())));
}

#[test]
fn test_web_open_signs_the_browser_in_and_never_shows_the_code() {
    let w = World::new();
    let daemon = Daemon::new();
    let browser = Arc::new(Browser::default());
    let mut ctx = w.ctx();
    browser_opens(&mut ctx, &browser, true, Some(daemon.clone()));
    let _serving = daemon.serve(&w.sock);
    let (rc, out) = run_lite(&["web", "open", "--wait", "5"], &ctx);
    assert_eq!(rc, 0, "{}", out.err);
    assert_eq!(out.out, format!("eve is open in your browser: http://localhost:{PORT}\n"));
    // the code reached the page and nowhere else on this side: not the screen or the page's name
    assert_eq!(*browser.codes.lock().unwrap(), vec![CANARY.to_string()]);
    let pages = browser.pages.lock().unwrap();
    assert!(pages[0].contains(&format!("location.replace(\"http://localhost:{PORT}/#login={CANARY}\")")));
    let paths = browser.paths.lock().unwrap();
    assert!(!out.out.contains(CANARY) && !out.err.contains(CANARY) && !paths[0].contains(CANARY));
    let page = PathBuf::from(&paths[0]);
    assert_eq!(*browser.modes.lock().unwrap(), vec![0o600]);
    assert_eq!(page.parent().unwrap(), w.root.join("t"));
    let name = page.file_name().unwrap().to_string_lossy().into_owned();
    assert!(name.starts_with("eve-open-") && name.ends_with(".html"));
    assert!(!page.exists() && w.pages().is_empty()); // deleted once the browser signed in
    let events = daemon.events();
    assert_eq!(events[0], "web_login");
    assert!(events[1..].iter().all(|e| e == "web_login_used") && events.len() > 1);
    assert!(daemon.seen.lock().unwrap()[1..].iter().all(|r| r["code"] == json!(CANARY)));
}

#[test]
fn test_web_open_waits_for_a_page_that_is_starting() {
    let w = World::new();
    let daemon = Daemon::with(json!(CANARY), json!(PORT), 3, &[]);
    let browser = Arc::new(Browser::default());
    let mut ctx = w.ctx();
    browser_opens(&mut ctx, &browser, true, Some(daemon.clone()));
    let _serving = daemon.serve(&w.sock);
    assert_eq!(run_lite(&["web", "open", "--wait", "5"], &ctx).0, 0);
    assert_eq!(daemon.events()[..4], ["web_login"; 4]);
}

#[test]
fn test_web_open_says_why_when_the_page_stays_down() {
    let w = World::new();
    let daemon = Daemon::with(json!(CANARY), json!(PORT), 10_000, &[]);
    let ctx = w.ctx();
    let _serving = daemon.serve(&w.sock);
    let (rc, out) = run_lite(&["web", "open", "--wait", "0.3"], &ctx);
    assert_eq!(rc, 1);
    assert!(out.err.contains(eve_lite::PAGE_DOWN));
    assert!(w.pages().is_empty());
}

#[test]
fn test_web_open_waits_for_a_loaded_service_then_says_eve_is_not_answering() {
    let w = World::with_host(World::launchctl());
    let ctx = w.ctx();
    let started = Instant::now();
    let (rc, out) = run_lite(&["web", "open", "--wait", "0.4"], &ctx);
    assert_eq!(rc, 1);
    assert!(started.elapsed() >= Duration::from_secs_f64(0.4));
    assert_eq!(out.out, "");
    assert!(out.err.contains(lite::LITE_NOT_ANSWERING));
    assert!(w.pages().is_empty());
}

#[test]
fn test_web_open_does_not_wait_for_an_eve_that_is_not_coming() {
    for left in ["nothing", "a stale socket file"] {
        let w = World::with_host(Host { launchctl: true, ..Host::default() });
        if left != "nothing" {
            std::fs::write(&w.sock, "").unwrap();
        }
        let started = Instant::now();
        let (rc, out) = run_lite(&["web", "open", "--wait", "30"], &w.ctx());
        assert_eq!(rc, 1);
        assert!(started.elapsed() < Duration::from_secs(5));
        let lists = w.launchd_calls().iter().filter(|c| c[1] == "list").count();
        assert_eq!(lists, 1); // asked launchd once
        assert!(out.err.contains(lite::LITE_NOT_LOADED));
        assert!(w.pages().is_empty());
    }
}

#[test]
fn test_web_open_asks_launchd_only_when_nothing_answers() {
    let w = World::with_host(World::launchctl());
    let daemon = Daemon::with(json!(CANARY), json!(PORT), 2, &[]);
    let browser = Arc::new(Browser::default());
    let mut ctx = w.ctx();
    browser_opens(&mut ctx, &browser, true, Some(daemon.clone()));
    let _serving = daemon.serve(&w.sock);
    assert_eq!(run_lite(&["web", "open", "--wait", "5"], &ctx).0, 0);
    assert!(w.launchd_calls().is_empty(), "launchd was asked");
}

#[test]
fn test_web_open_refuses_an_answer_that_is_not_a_code_and_a_port() {
    for (code, port) in [
        (json!("<script>x</script>"), json!(PORT)),
        (json!("short"), json!(PORT)),
        (Value::Null, json!(PORT)),
        (json!(CANARY), json!(0)),
        (json!(CANARY), json!(true)),
        (json!(CANARY), json!("48780")),
    ] {
        let w = World::new();
        let daemon = Daemon::with(code, port, 0, &[]);
        let _serving = daemon.serve(&w.sock);
        let (rc, out) = run_lite(&["web", "open", "--wait", "1"], &w.ctx());
        assert_eq!(rc, 1);
        assert!(out.err.contains(lite::LITE_NO_CODE));
        assert!(w.pages().is_empty());
    }
}

#[test]
fn test_web_open_deletes_the_page_when_the_code_runs_out() {
    let w = World::new();
    let browser = Arc::new(Browser::default()); // opens the page, never signs in
    let mut ctx = w.ctx();
    ctx.login_secs = 0.3;
    browser_opens(&mut ctx, &browser, true, None);
    let daemon = Daemon::new();
    let _serving = daemon.serve(&w.sock);
    let (rc, out) = run_lite(&["web", "open", "--wait", "1"], &ctx);
    assert_eq!(rc, 0);
    assert!(out.out.starts_with("eve is open in your browser:") && out.err.contains(lite::LITE_LOGIN_UNUSED));
    let paths = browser.paths.lock().unwrap();
    assert!(!paths.is_empty() && !Path::new(&paths[0]).exists());
}

#[test]
fn test_web_open_deletes_the_page_when_no_browser_opens() {
    let w = World::new();
    let browser = Arc::new(Browser::default());
    let mut ctx = w.ctx();
    browser_opens(&mut ctx, &browser, false, None);
    let daemon = Daemon::new();
    let _serving = daemon.serve(&w.sock);
    let (rc, out) = run_lite(&["web", "open", "--wait", "1"], &ctx);
    assert_eq!(rc, 1);
    assert_eq!(out.out, "");
    assert!(out.err.contains(lite::LITE_NO_BROWSER));
    let paths = browser.paths.lock().unwrap();
    assert!(!paths.is_empty() && !Path::new(&paths[0]).exists());
}

#[test]
fn test_the_login_page_goes_on_at_once_and_keeps_no_history() {
    let url = format!("http://localhost:{PORT}/#login={CANARY}");
    let page = String::from_utf8(lite::login_page(&url)).unwrap();
    assert!(page.contains(&format!("<meta http-equiv=\"refresh\" content=\"0; url={url}\">")));
    assert!(page.contains(&format!("location.replace(\"{url}\")")));
    assert!(page.contains("<meta name=\"referrer\" content=\"no-referrer\">"));
}

#[test]
fn test_the_page_goes_in_this_users_own_temporary_folder() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_string_lossy().into_owned();
    let env: HashMap<String, String> = [("TMPDIR".to_string(), dir.clone())].into();
    assert_eq!(lite::user_tmp(&env), Some(dir));
    if !cfg!(target_os = "macos") {
        return;
    }
    for unset in [None, Some(tmp.path().join("gone").to_string_lossy().into_owned())] {
        let env: HashMap<String, String> = unset.map(|u| ("TMPDIR".to_string(), u)).into_iter().collect();
        let mine = lite::user_tmp(&env).expect("the user's own folder");
        assert!(mine != "/tmp" && Path::new(&mine).is_dir());
        let meta = std::fs::metadata(&mine).unwrap();
        use std::os::unix::fs::MetadataExt;
        // SAFETY: getuid cannot fail.
        assert_eq!(meta.uid(), unsafe { libc::getuid() });
        assert_eq!(meta.mode() & 0o077, 0);
    }
}

fn launch_services(home: &Path, handlers: Vec<plist::Value>) {
    let path = home.join(lite::LAUNCH_SERVICES);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut d = plist::Dictionary::new();
    d.insert("LSHandlers".into(), plist::Value::Array(handlers));
    plist::Value::Dictionary(d).to_file_binary(&path).unwrap();
}

fn handler(scheme: &str, bundle: plist::Value) -> plist::Value {
    let mut d = plist::Dictionary::new();
    d.insert("LSHandlerURLScheme".into(), plist::Value::String(scheme.into()));
    d.insert("LSHandlerRoleAll".into(), bundle);
    plist::Value::Dictionary(d)
}

#[test]
fn test_the_page_opens_in_the_default_browser_by_its_bundle_id() {
    let w = World::new();
    let ctx = w.ctx();
    let page = w.home.join("eve-open-x.html").to_string_lossy().into_owned();
    assert_eq!(lite::default_browser(&w.home), None);
    assert!(lite::open_in_browser(&page, &ctx));
    assert_eq!(*w.programs.calls.lock().unwrap(), vec![s(&[lite::OPEN, &page])]); // no record: Safari
    launch_services(
        &w.home,
        vec![
            handler("mailto", plist::Value::String("com.apple.mail".into())),
            handler("http", plist::Value::String("com.google.chrome".into())),
        ],
    );
    w.programs.calls.lock().unwrap().clear();
    assert_eq!(lite::default_browser(&w.home).as_deref(), Some("com.google.chrome"));
    assert!(lite::open_in_browser(&page, &ctx));
    assert_eq!(*w.programs.calls.lock().unwrap(), vec![s(&[lite::OPEN, "-b", "com.google.chrome", &page])]);
    w.programs.calls.lock().unwrap().clear();
    *w.programs.fail.lock().unwrap() = vec!["/usr/bin/open -b".into()]; // that browser is gone: plain open
    assert!(lite::open_in_browser(&page, &ctx));
    assert_eq!(
        *w.programs.calls.lock().unwrap(),
        vec![s(&[lite::OPEN, "-b", "com.google.chrome", &page]), s(&[lite::OPEN, &page])]
    );
    *w.programs.fail.lock().unwrap() = vec!["/usr/bin/open".into()];
    assert!(!lite::open_in_browser(&page, &ctx));
}

#[test]
fn test_a_browser_record_that_is_no_bundle_id_is_not_passed_to_open() {
    for bundle in [
        plist::Value::String("-a".into()),
        plist::Value::String("--args".into()),
        plist::Value::String("not a bundle".into()),
        plist::Value::String("nodots".into()),
        plist::Value::String(String::new()),
        plist::Value::Integer(42.into()),
    ] {
        let tmp = tempfile::tempdir().unwrap();
        launch_services(tmp.path(), vec![handler("http", bundle)]);
        assert_eq!(lite::default_browser(tmp.path()), None);
        std::fs::write(tmp.path().join(lite::LAUNCH_SERVICES), b"not a plist").unwrap();
        assert_eq!(lite::default_browser(tmp.path()), None);
    }
}

// ---- install, uninstall, update ------------------------------------------------------------------------------

#[test]
fn test_lite_install_says_the_hooks_step_is_not_used() {
    let w = World::new();
    let ctx = w.ctx();
    let (rc1, out1) = run_lite(&["install"], &ctx);
    let (rc2, out2) = run_lite(&["uninstall"], &ctx);
    assert_eq!((rc1, rc2), (0, 0));
    assert!(out1.out.contains(lite::LITE_NO_HOOKS) && out2.out.contains(lite::LITE_NO_HOOKS_GONE));
}

#[test]
fn test_lite_install_service_installs_eve_lites_unit_only() {
    let w = World::with_host(Host { launchctl: true, ..Host::default() });
    let ctx = w.ctx();
    assert_eq!(run_lite(&["install", "--service"], &ctx).0, 0);
    assert!(ctx.launchd.lite_plist_path().unwrap().exists());
    assert_eq!(run_lite(&["uninstall", "--service"], &ctx).0, 0);
    assert!(!ctx.launchd.lite_plist_path().unwrap().exists());
    let calls = w.launchd_calls();
    assert!(!calls.is_empty());
    assert!(!calls.iter().any(|c| c.join(" ").contains(FULL_LABEL)), "full eve's service was touched");
    assert!(!w.home.join("Library/LaunchAgents").join(format!("{FULL_LABEL}.plist")).exists());
}

#[test]
fn test_lite_install_service_is_for_a_mac() {
    let w = World::new();
    let mut ctx = w.ctx();
    ctx.platform = "linux".into();
    let (rc, out) = run_lite(&["install", "--service"], &ctx);
    assert_eq!(rc, 2);
    assert!(out.err.contains("eve lite runs on a Mac") && out.err.contains("'linux'"));
    assert!(w.launchd_calls().is_empty());
}

#[test]
fn test_lite_update_refuses_with_the_install_line() {
    let w = World::new();
    for words in [&["update"][..], &["update", "-y"]] {
        let (rc, out) = run_lite(words, &w.ctx());
        assert_eq!(rc, 2);
        assert!(out.err.contains(lite::LITE_INSTALL_LINE) && out.err.contains("does not update itself"));
    }
}

#[test]
fn test_lite_memory_forget_is_the_memory_stores_own() {
    let w = World::new();
    let mut ctx = w.ctx();
    let opened = Arc::new(AtomicUsize::new(0));
    let o = opened.clone();
    ctx.memory.open = Box::new(move |_| {
        o.fetch_add(1, Ordering::SeqCst);
        None
    });
    assert_eq!(run_lite(&["memory", "forget", "blue pot"], &ctx).0, 1); // the store's own answer: not open
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    let (rc, out) = run_lite(&["memory", "reindex"], &ctx); // not in eve lite
    assert_eq!(rc, 2);
    assert!(out.err.contains("invalid choice: 'reindex'"));
}

// ---- status --------------------------------------------------------------------------------------------------

fn status() -> Value {
    json!({"ok": true, "state": "READY", "openrouter": true, "chat": true, "composio": false, "apps": false,
           "voice": true, "page": true, "port": PORT, "lan": true, "lan_port": 48781, "local_network": "blocked",
           "restarting": false, "firewall": {"exec:git": 3}})
}

fn status_rows(out: &str) -> HashMap<String, String> {
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines[0], "eve lite on this Mac:");
    lines[1..]
        .iter()
        .map(|l| {
            let (name, value) = l.trim_end().rsplit_once(char::is_whitespace).unwrap();
            (name.trim().to_string(), value.to_string())
        })
        .collect()
}

fn rows(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

#[test]
fn test_lite_status_is_yes_or_no_for_each_part() {
    let w = World::with_host(World::launchctl());
    std::fs::create_dir_all(w.home.join("Library/LaunchAgents")).unwrap();
    std::fs::write(w.home.join(format!("Library/LaunchAgents/{LITE_LABEL}.plist")), "x").unwrap();
    let mut ctx = w.ctx();
    ctx.env.insert("OPENROUTER_API_KEY".into(), CANARY.into());
    let daemon = Daemon::answering(&[("status", status())]);
    let _serving = daemon.serve(&w.sock);
    let (rc, out) = run_lite(&["status"], &ctx);
    assert_eq!(rc, 0);
    assert_eq!(
        status_rows(&out.out),
        rows(&[
            ("login service installed", "yes"),
            ("login service loaded", "yes"),
            ("eve answering", "yes"),
            ("OpenRouter signed in", "yes"),
            ("Composio signed in", "no"),
            ("chat on", "yes"),
            ("voice ready", "yes"),
            ("phone access on", "yes"),
        ])
    );
    assert!(!out.out.chars().any(|c| c.is_ascii_digit()) && !out.out.contains(CANARY));
}

#[test]
fn test_lite_status_reads_the_sign_ins_from_this_mac_when_eve_is_down() {
    let w = World::with_host(Host { launchctl: true, ..Host::default() });
    std::fs::create_dir_all(w.home.join("Library/LaunchAgents")).unwrap();
    std::fs::write(w.home.join(format!("Library/LaunchAgents/{LITE_LABEL}.plist")), "x").unwrap();
    std::fs::write(w.home.join(".config/eve-bridge/composio-connect.json"), "{}").unwrap();
    let mut ctx = w.ctx();
    // the env file's key, as cli.main loads it before the profile
    ctx.env.insert("OPENROUTER_API_KEY".into(), CANARY.into());
    let (rc, out) = run_lite(&["status"], &ctx);
    assert_eq!(rc, 0);
    assert_eq!(
        status_rows(&out.out),
        rows(&[
            ("login service installed", "yes"),
            ("login service loaded", "no"),
            ("eve answering", "no"),
            ("OpenRouter signed in", "yes"),
            ("Composio signed in", "yes"),
            ("chat on", "no"),
            ("voice ready", "no"),
            ("phone access on", "no"),
        ])
    );
    assert!(!out.out.contains(CANARY));
}

// ---- web devices, revoke, lan --------------------------------------------------------------------------------

#[test]
fn test_web_devices_lists_each_browser_and_no_terminal_codes() {
    let w = World::new();
    let devices = json!({"ok": true, "devices": [
        {"id": "a1b2c3d4", "label": "iPhone · Safari", "host": "kitchen-mac.local", "created": 1760000000,
         "last_seen": 1760003600},
        {"id": "0f0f0f0f", "label": "Android\u{1b}[31m · Chrome", "host": "192.168.1.20", "created": 1760007200,
         "last_seen": null},
    ]});
    let daemon = Daemon::answering(&[("devices", devices)]);
    let _serving = daemon.serve(&w.sock);
    let (rc, out) = run_lite(&["web", "devices"], &w.ctx());
    assert_eq!(rc, 0);
    let o = out.out;
    assert!(o.starts_with("2 browsers can open eve:"));
    assert!(o.contains("a1b2c3d4  iPhone · Safari") && o.contains("kitchen-mac.local") && o.contains("Android[31m · Chrome"));
    assert!(!o.contains('\u{1b}') && o.contains("last seen ?"));
    assert!(o.contains("eve-bridge web revoke <id>"));
}

#[test]
fn test_web_devices_with_none_says_how_to_add_one() {
    for listed in [json!([]), Value::Null, json!({"id": "a1b2c3d4"}), json!("a1b2c3d4"), json!(["a1b2c3d4"])] {
        let w = World::new();
        let daemon = Daemon::answering(&[("devices", json!({"ok": true, "devices": listed}))]);
        let _serving = daemon.serve(&w.sock);
        let (rc, out) = run_lite(&["web", "devices"], &w.ctx());
        assert_eq!(rc, 0);
        assert!(out.out.contains("No browser can open eve yet"));
    }
}

#[test]
fn test_web_revoke_signs_browsers_out_by_id_or_all() {
    let w = World::new();
    let ctx = w.ctx();
    let daemon = Daemon::answering(&[("revoke", json!({"ok": true, "revoked": ["a1b2c3d4"]}))]);
    let _serving = daemon.serve(&w.sock);
    let mut out = String::new();
    let mut go = |words: &[&str]| {
        let (rc, o) = run_lite(words, &ctx);
        out.push_str(&o.out);
        rc
    };
    assert_eq!(go(&["web", "revoke", "A1B2C3D4"]), 0);
    assert_eq!(go(&["web", "revoke", "all"]), 0);
    daemon.answers.lock().unwrap().insert("revoke".into(), json!({"ok": true, "revoked": []}));
    assert_eq!(go(&["web", "revoke", "a1b2c3d4"]), 1); // no such browser: nothing changed
    assert_eq!(go(&["web", "revoke", "all"]), 0);
    let ids: Vec<Value> = daemon.seen.lock().unwrap().iter().map(|r| r["id"].clone()).collect();
    assert_eq!(ids, vec![json!("a1b2c3d4"), json!("all"), json!("a1b2c3d4"), json!("all")]);
    assert_eq!(out.matches("Signed out of eve: a1b2c3d4").count(), 2);
    assert!(out.contains("No browser has the id a1b2c3d4"));
}

#[test]
fn test_web_revoke_reads_only_a_list_of_ids() {
    let w = World::new();
    let daemon = Daemon::answering(&[("revoke", json!({"ok": true, "revoked": "a1b2c3d4"}))]);
    let _serving = daemon.serve(&w.sock);
    let (rc, out) = run_lite(&["web", "revoke", "a1b2c3d4"], &w.ctx());
    assert_eq!(rc, 1);
    assert!(!out.out.contains("Signed out"));
}

#[test]
fn test_web_revoke_asks_nothing_for_an_id_that_is_none() {
    for target in ["nope", "a1b2c3d", "a1b2c3d4e", "../x", ""] {
        let w = World::new();
        let daemon = Daemon::new();
        let _serving = daemon.serve(&w.sock);
        let (rc, out) = run_lite(&["web", "revoke", target], &w.ctx());
        assert_eq!(rc, 2, "{target}");
        assert!(daemon.seen.lock().unwrap().is_empty() && out.err.contains("give a browser's id"));
    }
}

#[test]
fn test_web_lan_turns_phone_access_on_and_off() {
    let w = World::new();
    let ctx = w.ctx();
    let daemon = Daemon::answering(&[("lan", json!({"ok": true, "lan": true}))]);
    let _serving = daemon.serve(&w.sock);
    let (mut out, mut err) = (String::new(), String::new());
    let mut go = |words: &[&str]| {
        let (rc, o) = run_lite(words, &ctx);
        out.push_str(&o.out);
        err.push_str(&o.err);
        rc
    };
    assert_eq!(go(&["web", "lan", "on"]), 0);
    assert_eq!(go(&["web", "lan", "off"]), 1); // it answered that the door is still open
    daemon.answers.lock().unwrap().insert("lan".into(), json!({"ok": true, "lan": false}));
    assert_eq!(go(&["web", "lan", "off"]), 0);
    daemon.answers.lock().unwrap().insert("lan".into(), json!({"ok": false, "error": eve_lite_web::LAN_FAILED}));
    assert_eq!(go(&["web", "lan", "on"]), 1);
    let ons: Vec<Value> = daemon.seen.lock().unwrap().iter().map(|r| r["on"].clone()).collect();
    assert_eq!(ons, vec![json!(true), json!(false), json!(false), json!(true)]);
    assert!(out.contains("Phone access is on") && out.contains("Phone access is off"));
    assert!(err.contains(eve_lite_web::LAN_FAILED));
}

#[test]
fn test_a_refusal_that_reads_as_a_key_is_never_echoed() {
    let w = World::new();
    let leak = format!("failed for {CANARY}-{CANARY}"); // one run of 43 URL-safe characters
    let daemon = Daemon::answering(&[("lan", json!({"ok": false, "error": leak}))]);
    let _serving = daemon.serve(&w.sock);
    let (rc, out) = run_lite(&["web", "lan", "on"], &w.ctx());
    assert_eq!(rc, 1);
    assert!(!out.err.contains(&leak) && out.err.contains(lite::LITE_REFUSED));
}

#[test]
fn test_web_commands_say_so_when_eve_is_not_running() {
    let w = World::new();
    for words in [&["web", "devices"][..], &["web", "revoke", "all"], &["web", "lan", "on"]] {
        let (rc, out) = run_lite(words, &w.ctx());
        assert_eq!(rc, 1);
        assert!(out.err.contains(lite::LITE_NOT_ANSWERING));
    }
}

// ---- doctor --------------------------------------------------------------------------------------------------

struct Healthy {
    env: PathBuf,
    tls: PathBuf,
    plist: PathBuf,
    web: PathBuf,
}

/// An eve lite install as it should be, in `w.home`: the env file, the login service, the page and the private
/// files.
fn healthy(w: &World, ctx: &mut Ctx) -> Healthy {
    let folder = w.home.join(".config/eve-bridge");
    let env = folder.join("env");
    std::fs::write(&env, format!("EVE_PROFILE=lite\nOPENROUTER_API_KEY={CANARY}\n")).unwrap();
    for name in ["composio-connect.json", "web-devices.json", "web.json"] {
        std::fs::write(folder.join(name), "{}").unwrap();
    }
    let tls = folder.join("web-tls");
    std::fs::create_dir(&tls).unwrap();
    std::fs::set_permissions(&tls, std::fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["ca.key", "leaf.key", "ca.pem", "leaf.pem"] {
        std::fs::write(tls.join(name), "x").unwrap();
    }
    for path in [
        env.clone(),
        folder.join("composio-connect.json"),
        folder.join("web-devices.json"),
        folder.join("web.json"),
        tls.join("ca.key"),
        tls.join("leaf.key"),
    ] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let plist = ctx.launchd.lite_plist_path().unwrap();
    std::fs::create_dir_all(plist.parent().unwrap()).unwrap();
    std::fs::write(&plist, ctx.launchd.build_lite_plist().unwrap()).unwrap();
    let web = w.home.join("web");
    std::fs::create_dir(&web).unwrap();
    std::fs::write(web.join("index.html"), "<!doctype html>").unwrap();
    ctx.env.insert("EVE_WEB_DIR".into(), web.to_string_lossy().into_owned());
    ctx.env.insert("OPENROUTER_API_KEY".into(), CANARY.into());
    Healthy { env, tls, plist, web }
}

fn healthy_status() -> Value {
    let mut st = status();
    st["composio"] = json!(true);
    st["local_network"] = json!("ok");
    st["firewall"] = json!({});
    st
}

fn doctor(w: &World, ctx: &Ctx, status: Option<Value>) -> (i32, String) {
    let _serving = status.map(|st| Daemon::answering(&[("status", st)]).serve(&w.sock));
    let (rc, out) = run_lite(&["doctor"], ctx);
    (rc, out.out)
}

fn marks(out: &str) -> HashMap<String, String> {
    let lines: Vec<&str> = out.lines().collect();
    lines[1..lines.len() - 1]
        .iter()
        .map(|l| (l[6..].split(':').next().unwrap().to_string(), l[2..4].to_string()))
        .collect()
}

#[test]
fn test_doctor_on_a_healthy_install_finds_nothing_to_fix() {
    let w = World::with_host(World::launchctl());
    let mut ctx = w.ctx();
    healthy(&w, &mut ctx);
    let (rc, out) = doctor(&w, &ctx, Some(healthy_status()));
    assert_eq!(rc, 0, "{out}");
    assert_eq!(out.lines().last().unwrap(), "Nothing to fix.");
    assert!(!out.contains(lite::FIX));
    let m = marks(&out);
    let with = |mark: &str| -> std::collections::BTreeSet<String> {
        m.iter().filter(|(_, v)| v.as_str() == mark).map(|(k, _)| k.clone()).collect()
    };
    let want: std::collections::BTreeSet<String> = [
        "profile", "login service", "eve", "OpenRouter", "Composio", "the Mac's page", "phone access",
        "Local Network", "voice", "firewall", "web page", "private files",
    ]
    .iter()
    .map(|x| x.to_string())
    .collect();
    assert_eq!(with(lite::OK), want);
    let info: std::collections::BTreeSet<String> =
        [format!("eve-bridge {}", eve_meta::VERSION), "Xcode's command line tools".to_string()].into();
    assert_eq!(with(lite::INFO), info);
    assert!(out.contains(&format!("http://localhost:{PORT}")) && !out.contains(CANARY));
}

#[test]
fn test_doctor_reports_a_turned_down_openrouter_key() {
    let w = World::with_host(World::launchctl());
    let mut ctx = w.ctx();
    healthy(&w, &mut ctx);
    let mut st = healthy_status();
    st["openrouter_rejected"] = json!(true);
    let (rc, out) = doctor(&w, &ctx, Some(st));
    assert_eq!(rc, 1);
    assert_eq!(marks(&out)["OpenRouter"], lite::FIX);
    assert!(out.contains("turned down") && out.contains("Sign in with OpenRouter again"));
    assert_eq!(out.lines().last().unwrap(), "1 to fix, marked !!.");
}

#[test]
fn test_doctor_reports_a_blocked_local_network() {
    let w = World::with_host(World::launchctl());
    let mut ctx = w.ctx();
    healthy(&w, &mut ctx);
    let mut st = healthy_status();
    st["local_network"] = json!("blocked");
    let (rc, out) = doctor(&w, &ctx, Some(st));
    assert_eq!(rc, 1);
    assert_eq!(marks(&out)["Local Network"], lite::FIX);
    assert!(out.contains("Local Network: blocked") && out.contains("Privacy & Security, Local Network"));
    assert_eq!(out.lines().last().unwrap(), "1 to fix, marked !!.");
}

#[test]
fn test_doctor_names_what_the_firewall_refused() {
    let w = World::with_host(World::launchctl());
    let mut ctx = w.ctx();
    healthy(&w, &mut ctx);
    let mut st = healthy_status();
    st["firewall"] = json!({"exec:git": 2, "path:~/Documents": 1});
    let (rc, out) = doctor(&w, &ctx, Some(st));
    assert_eq!(rc, 0);
    assert!(out.contains("firewall: refused git (2), ~/Documents (1).") && marks(&out)["firewall"] == lite::INFO);
}

#[test]
fn test_doctor_with_eve_down_says_where_to_look() {
    let w = World::with_host(World::launchctl());
    let mut ctx = w.ctx();
    healthy(&w, &mut ctx);
    let (rc, out) = doctor(&w, &ctx, None);
    assert_eq!(rc, 1);
    assert!(out.contains("eve: not answering on this Mac") && marks(&out)["OpenRouter"] == lite::OK);
    assert!(!out.contains("Local Network")); // only eve itself knows
}

#[test]
fn test_doctor_finds_what_is_wrong_on_this_mac() {
    let w = World::with_host(World::launchctl());
    let mut ctx = w.ctx();
    let parts = healthy(&w, &mut ctx);
    std::fs::write(&parts.env, format!("OPENROUTER_API_KEY={CANARY}\n")).unwrap(); // the profile line gone
    std::fs::set_permissions(&parts.env, std::fs::Permissions::from_mode(0o644)).unwrap(); // readable by others
    std::fs::set_permissions(parts.tls.join("ca.key"), std::fs::Permissions::from_mode(0o640)).unwrap();
    std::fs::remove_file(parts.web.join("index.html")).unwrap();
    let full = ctx.launchd.agent_path(FULL_LABEL).unwrap();
    std::fs::write(&full, b"full eve's").unwrap();
    let (rc, out) = doctor(&w, &ctx, Some(healthy_status()));
    assert_eq!(rc, 1);
    let m = marks(&out);
    assert!(m["profile"] == lite::FIX && m["private files"] == lite::FIX && m["web page"] == lite::FIX);
    assert!(out.contains("~/.config/eve-bridge/env, ~/.config/eve-bridge/web-tls/ca.key"));
    assert!(out.contains(&format!("full eve's login service is installed for this user ({FULL_LABEL})")));
    assert_eq!(out.lines().last().unwrap(), "4 to fix, marked !!.");
    assert!(!out.contains(CANARY));
}

#[test]
fn test_doctor_checks_the_plist_pins_the_profile() {
    let w = World::with_host(World::launchctl());
    let mut ctx = w.ctx();
    let parts = healthy(&w, &mut ctx);
    let mut unit = plist::Value::from_file(&parts.plist).unwrap().into_dictionary().unwrap();
    unit.get_mut("EnvironmentVariables").unwrap().as_dictionary_mut().unwrap().remove("EVE_PROFILE");
    plist::Value::Dictionary(unit).to_file_xml(&parts.plist).unwrap();
    let (rc, out) = doctor(&w, &ctx, Some(healthy_status()));
    assert!(rc == 1 && out.contains("login service: it does not pin EVE_PROFILE=lite"), "{out}");
    let gone = w.home.join("no-such-python").to_string_lossy().into_owned();
    let hand_edited = vec![
        {
            let mut d = plist::Dictionary::new();
            d.insert("a".into(), plist::Value::Integer(1.into()));
            plist::Value::Dictionary(d)
        },
        plist::Value::String("python".into()),
        plist::Value::Array(vec![]),
        plist::Value::Array(vec![plist::Value::String(gone)]),
    ];
    for argv in hand_edited {
        let mut unit = plist::Value::from_reader(std::io::Cursor::new(ctx.launchd.build_lite_plist().unwrap()))
            .unwrap()
            .into_dictionary()
            .unwrap();
        unit.insert("ProgramArguments".into(), argv);
        plist::Value::Dictionary(unit).to_file_xml(&parts.plist).unwrap();
        let (rc, out) = doctor(&w, &ctx, Some(healthy_status()));
        assert!(rc == 1 && out.contains("login service: the Python it starts is gone"), "{out}");
    }
}

// ---- lite uninstall ------------------------------------------------------------------------------------------

struct Installed {
    plist: PathBuf,
    lite_home: PathBuf,
    log: PathBuf,
    app: PathBuf,
    env: PathBuf,
    kept: PathBuf,
    memory: PathBuf,
    full: PathBuf,
}

/// eve lite installed in `w.home`, as install-lite.sh leaves it, with a memory to keep or delete.
fn installed(w: &World, ctx: &Ctx, env: Option<&str>, app: bool, full: bool) -> Installed {
    let h = &w.home;
    let p = Installed {
        plist: ctx.launchd.lite_plist_path().unwrap(),
        lite_home: h.join(".eve-lite"),
        log: h.join("Library/Logs/eve-lite.log"),
        app: h.join("Applications/Eve.app"),
        env: h.join(".config/eve-bridge/env"),
        kept: h.join(".config/eve-bridge").join(lite::LITE_KEPT),
        memory: h.join(".config/eve-bridge/memory/notes.md"),
        full: ctx.launchd.agent_path(FULL_LABEL).unwrap(),
    };
    std::fs::create_dir_all(p.plist.parent().unwrap()).unwrap();
    std::fs::write(&p.plist, ctx.launchd.build_lite_plist().unwrap()).unwrap();
    std::fs::create_dir_all(p.lite_home.join("venv/bin")).unwrap();
    std::fs::write(p.lite_home.join("venv/bin/eve-bridge"), "#!/bin/sh\n").unwrap();
    std::fs::create_dir_all(p.log.parent().unwrap()).unwrap();
    std::fs::write(&p.log, "a log line\n").unwrap();
    if app {
        std::fs::create_dir_all(p.app.join("Contents")).unwrap();
    }
    std::fs::create_dir_all(p.memory.parent().unwrap()).unwrap();
    std::fs::write(&p.memory, "what eve remembers\n").unwrap();
    if let Some(text) = env {
        std::fs::write(&p.env, text).unwrap();
        std::fs::set_permissions(&p.env, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    if full {
        std::fs::write(&p.full, b"full eve's").unwrap();
    }
    p
}

fn default_env() -> String {
    format!("EVE_PROFILE=lite\nOPENROUTER_API_KEY={CANARY}\n")
}

fn eve_app_script(home: &Path) -> String {
    format!("do shell script \"EVE_PROFILE=lite PYTHONNOUSERSITE=1 '{}/.eve-lite/venv/bin/eve-bridge' web open\"", home.display())
}

fn uninstall(ctx: &Ctx, dry_run: bool, answer: Option<&str>) -> (i32, Captured, Arc<Mutex<Vec<String>>>) {
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let (mut io, seen) = capture(no_post(), None);
    if let Some(a) = answer {
        let (a, q) = (a.to_string(), asked.clone());
        io.ask = Box::new(move |question| {
            q.lock().unwrap().push(question.to_string());
            Ok(a.clone())
        });
        io.stdin_tty = true;
    }
    let rc = lite::lite_uninstall(dry_run, ctx, &io);
    (rc, Captured::take(&seen), asked)
}

fn lexists(p: &Path) -> bool {
    std::fs::symlink_metadata(p).is_ok()
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

#[test]
fn test_lite_uninstall_keeps_the_memory_on_no_and_the_installer_takes_the_folder_back() {
    let w = World::with_host(World::launchctl());
    let ctx = w.ctx();
    let p = installed(&w, &ctx, Some(&default_env()), true, false);
    *w.programs.script.lock().unwrap() = eve_app_script(&w.home);
    let (rc, out, asked) = uninstall(&ctx, false, Some("n"));
    assert_eq!(rc, 0, "{}", out.err);
    let asked = asked.lock().unwrap();
    assert!(asked.len() == 1 && asked[0].contains("~/.config/eve-bridge"));
    for gone in [&p.plist, &p.lite_home, &p.log, &p.app] {
        assert!(!lexists(gone), "{}", gone.display());
    }
    assert_eq!(std::fs::read_to_string(&p.memory).unwrap(), "what eve remembers\n");
    assert_eq!(std::fs::read_to_string(&p.env).unwrap(), format!("OPENROUTER_API_KEY={CANARY}\n"));
    assert_eq!(std::fs::metadata(&p.env).unwrap().permissions().mode() & 0o777, 0o600);
    let digest = sha256(&std::fs::read(&p.env).unwrap());
    assert!(std::fs::read_to_string(&p.kept).unwrap().lines().any(|l| l == format!("env_sha256={digest}")));
    assert_eq!(std::fs::metadata(&p.kept).unwrap().permissions().mode() & 0o777, 0o600);
    // SAFETY: getuid cannot fail.
    let uid = unsafe { libc::getuid() };
    assert!(w.launchd_calls().contains(&s(&["launchctl", "bootout", &format!("gui/{uid}/{LITE_LABEL}")])));
    assert!(!w.launchd_calls().iter().any(|c| c.join(" ").contains(FULL_LABEL)));
    assert!(out.out.lines().last().unwrap().ends_with("the install line takes them back.") && !out.out.contains(CANARY));
    // kept_by_lite reads the folder as the one eve lite kept; a changed env file afterwards is not
    assert!(lite::kept_by_lite(&p.kept, &p.env));
    // install-lite.sh reads the folder as the one eve lite kept: past its env-file check, to its release check
    let installer_runs = installer_available();
    if installer_runs {
        let (_, _, err) = installer(&w, &["--dry-run"]);
        assert!(err.contains("is not from a release"), "{err}");
    }
    // control: the same folder with its env file changed afterwards is not eve lite's to take back
    std::fs::write(&p.env, format!("OPENROUTER_API_KEY={CANARY}\nEVE_SOMETHING=1\n")).unwrap();
    assert!(!lite::kept_by_lite(&p.kept, &p.env));
    if installer_runs {
        let (_, _, err) = installer(&w, &["--dry-run"]);
        assert!(err.contains("changed after eve lite was uninstalled"), "{err}");
    }
}

/// install-lite.sh at the repository's root.
fn installer_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../install-lite.sh")
}

/// Whether install-lite.sh can run here (the Python's `needs_installer`: a Mac, the script, /bin/bash).
fn installer_available() -> bool {
    cfg!(target_os = "macos") && installer_path().is_file() && Path::new("/bin/bash").exists()
}

/// install-lite.sh itself, run in the world's HOME: only its dry runs, which write nothing and start no service.
/// (exit code, stdout, stderr)
fn installer(w: &World, args: &[&str]) -> (i32, String, String) {
    assert!(args.contains(&"--dry-run"));
    let done = std::process::Command::new("/bin/bash")
        .arg(installer_path())
        .args(args)
        .env_clear()
        .env("HOME", &w.home)
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("TMPDIR", w.root.join("t"))
        .stdin(std::process::Stdio::null())
        .output()
        .expect("bash runs install-lite.sh");
    (
        done.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&done.stdout).into_owned(),
        String::from_utf8_lossy(&done.stderr).into_owned(),
    )
}

#[test]
fn test_lite_uninstall_of_an_env_file_holding_only_the_profile() {
    let w = World::with_host(World::launchctl());
    let ctx = w.ctx();
    let p = installed(&w, &ctx, Some("EVE_PROFILE=lite\n"), false, false);
    assert_eq!(uninstall(&ctx, false, Some("")).0, 0);
    assert!(!p.env.exists());
    assert!(std::fs::read_to_string(&p.kept).unwrap().lines().any(|l| l == "env_sha256=none"));
    assert!(lite::kept_by_lite(&p.kept, &p.env));
    if installer_available() {
        let (_, _, err) = installer(&w, &["--dry-run"]);
        assert!(err.contains("is not from a release"), "{err}");
    }
}

#[test]
fn test_lite_uninstall_deletes_the_memory_on_yes() {
    let w = World::with_host(World::launchctl());
    let ctx = w.ctx();
    let p = installed(&w, &ctx, Some(&default_env()), true, false);
    *w.programs.script.lock().unwrap() = eve_app_script(&w.home);
    let (rc, out, _) = uninstall(&ctx, false, Some("yes"));
    assert_eq!(rc, 0);
    assert!(!w.home.join(".config/eve-bridge").exists());
    for gone in [&p.plist, &p.lite_home, &p.log, &p.app] {
        assert!(!lexists(gone));
    }
    assert_eq!(out.out.lines().last().unwrap(), "eve lite is uninstalled, with its memory and settings.");
}

#[test]
fn test_lite_uninstall_with_no_terminal_keeps_the_memory() {
    let w = World::with_host(World::launchctl());
    let ctx = w.ctx();
    let p = installed(&w, &ctx, Some(&default_env()), true, false);
    let (rc, _, asked) = uninstall(&ctx, false, None); // not a terminal: never read
    assert_eq!(rc, 0);
    assert!(asked.lock().unwrap().is_empty());
    assert!(p.memory.exists() && p.kept.exists());
}

#[test]
fn test_lite_uninstall_beside_full_eve_never_offers_the_folder() {
    let w = World::with_host(World::launchctl());
    let ctx = w.ctx();
    let env = format!("EVE_PROFILE=lite\nEVE_MEMORY=1\nOPENROUTER_API_KEY={CANARY}\n");
    let p = installed(&w, &ctx, Some(&env), true, true);
    std::fs::write(&p.kept, "env_sha256=none\n").unwrap();
    let (rc, out, asked) = uninstall(&ctx, false, Some("yes"));
    assert_eq!(rc, 0);
    assert!(asked.lock().unwrap().is_empty(), "offered full eve's folder");
    assert_eq!(std::fs::read(&p.full).unwrap(), b"full eve's");
    assert_eq!(std::fs::read_to_string(&p.env).unwrap(), format!("EVE_MEMORY=1\nOPENROUTER_API_KEY={CANARY}\n"));
    assert!(!p.kept.exists() && p.memory.exists());
    assert!(!w.launchd_calls().iter().any(|c| c.join(" ").contains(FULL_LABEL)));
    assert_eq!(out.out.lines().last().unwrap(), "eve lite is uninstalled. Full eve stays as it is.");
}

#[test]
fn test_lite_uninstall_leaves_an_eve_app_it_did_not_make() {
    let w = World::with_host(World::launchctl());
    let ctx = w.ctx();
    let p = installed(&w, &ctx, Some(&default_env()), true, false);
    *w.programs.script.lock().unwrap() = "do shell script \"open -a Notes\"".into();
    assert_eq!(uninstall(&ctx, false, Some("n")).0, 0);
    assert!(p.app.is_dir());
    assert!(w.programs.calls.lock().unwrap().contains(&s(&[lite::OSADECOMPILE, &p.app.to_string_lossy()])));
}

#[test]
fn test_lite_uninstall_removes_links_not_what_they_point_to() {
    let w = World::with_host(World::launchctl());
    let ctx = w.ctx();
    let p = installed(&w, &ctx, Some(&default_env()), false, false);
    let elsewhere = w.root.join("elsewhere");
    let (real_app, real_log) = (elsewhere.join("Eve.app"), elsewhere.join("kept.log"));
    std::fs::create_dir_all(real_app.join("Contents")).unwrap();
    std::fs::write(&real_log, "someone's log\n").unwrap();
    std::fs::create_dir_all(p.app.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&real_app, &p.app).unwrap();
    std::fs::remove_file(&p.log).unwrap();
    std::os::unix::fs::symlink(&real_log, &p.log).unwrap();
    *w.programs.script.lock().unwrap() = eve_app_script(&w.home);
    assert_eq!(uninstall(&ctx, false, Some("n")).0, 0);
    assert!(!lexists(&p.app) && !lexists(&p.log));
    assert!(real_app.join("Contents").is_dir());
    assert_eq!(std::fs::read_to_string(&real_log).unwrap(), "someone's log\n");
}

#[test]
fn test_lite_uninstall_says_what_it_could_not_remove() {
    let w = World::with_host(World::launchctl());
    let ctx = w.ctx();
    let p = installed(&w, &ctx, Some(&default_env()), false, false);
    // ~/.eve-lite stays: its folder cannot be taken out of a HOME that is read-only
    std::fs::set_permissions(&w.home, std::fs::Permissions::from_mode(0o555)).unwrap();
    let (rc, out, _) = uninstall(&ctx, false, Some("n"));
    std::fs::set_permissions(&w.home, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(rc, 1);
    assert!(out.err.contains("~/.eve-lite was not removed (PermissionError)"), "{}", out.err);
    assert!(!p.plist.exists() && p.kept.exists()); // the rest still went, and the folder was marked
    assert!(out.out.lines().last().unwrap().ends_with("the install line takes them back."));
}

fn tree(home: &Path) -> Vec<(String, u32, Vec<u8>)> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, u32, Vec<u8>)>) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        entries.sort();
        for p in entries {
            let meta = std::fs::symlink_metadata(&p).unwrap();
            let bytes = if meta.is_file() { std::fs::read(&p).unwrap() } else { Vec::new() };
            out.push((p.strip_prefix(base).unwrap().to_string_lossy().into_owned(), meta.permissions().mode(), bytes));
            if meta.is_dir() {
                walk(&p, base, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(home, home, &mut out);
    out
}

#[test]
fn test_lite_uninstall_dry_run_writes_nothing_and_decides_as_the_installer() {
    for state in ["lite", "kept", "full", "none"] {
        let w = World::with_host(World::launchctl());
        let ctx = w.ctx();
        let env = match state {
            "lite" | "full" => "EVE_PROFILE=lite\n",
            _ => "OPENROUTER_API_KEY=x\n",
        };
        let p = installed(&w, &ctx, Some(env), false, state == "full");
        if state == "kept" {
            std::fs::write(&p.kept, format!("env_sha256={}\n", sha256(env.as_bytes()))).unwrap();
        }
        let before = tree(&w.home);
        let (rc, out, _) = uninstall(&ctx, true, None);
        assert_eq!(rc, 0);
        assert_eq!(tree(&w.home), before, "{state}");
        assert_eq!(out.out.lines().last().unwrap(), "Nothing was written.");
        for path in ["~/Library/LaunchAgents/com.github.eve-bridge.lite.plist", "~/.eve-lite", "~/Library/Logs/eve-lite.log"] {
            assert!(out.out.contains(&format!("  {path}\n")), "{state}: {path}");
        }
        let asks = out.out.contains("Then it would ask before deleting ~/.config/eve-bridge");
        assert_eq!(asks, state == "lite" || state == "kept", "{state}");
        assert!(w.launchd_calls().is_empty() || !w.launchd_calls().iter().any(|c| c[1] == "bootout"));
        if installer_available() {
            let (rc, said, err) = installer(&w, &["--uninstall", "--dry-run"]);
            assert_eq!(rc, 0, "{state}: {err}");
            assert_eq!(said.contains("Then it would ask before deleting ~/.config/eve-bridge"), asks, "{state}: {said}");
            assert_eq!(tree(&w.home), before, "{state}");
        }
    }
}

#[test]
fn test_plain_lines_and_shown_text() {
    assert_eq!(lite::shown(&json!("a\u{1b}[31mb\u{200b}c"), 300), "a[31mbc");
    assert_eq!(lite::shown(&Value::Null, 8), "None");
    assert_eq!(lite::plain_line(&json!("  two\n words "), "x"), "two words");
    assert_eq!(lite::plain_line(&json!(5), "fallback"), "fallback");
    assert_eq!(lite::plain_line(&json!("abcdefghijklmnopqrstuvwxyz0123"), "fallback"), "fallback");
    assert!(lite::is_port(&json!(1)) && !lite::is_port(&json!(65536)) && !lite::is_port(&json!(80.0)));
    assert_eq!(lite::when(&json!(true)), "?");
    assert_eq!(lite::when(&json!(-1)), "?");
}

// ---- memory forget, dream, reindex ---------------------------------------------------------------------------

const SECRET: &str = "the spare key is under the blue pot";

struct MemoryWorld {
    _tmp: tempfile::TempDir,
    cfg: eve_records::RecallConfig,
}

impl MemoryWorld {
    fn new() -> MemoryWorld {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = eve_records::RecallConfig::new(tmp.path().join("memory"), tmp.path().join("notes"));
        MemoryWorld { _tmp: tmp, cfg }
    }

    fn transcripts(&self) -> Arc<eve_transcripts::Transcripts> {
        Arc::new(eve_transcripts::Transcripts::new(
            eve_transcripts::TranscriptConfig::new(true, self.cfg.transcripts_dir()),
            Some(self.cfg.archive_dir()),
            None,
        ))
    }

    fn store(&self, index: Option<Arc<dyn StoreIndex>>) -> Store {
        Store { cfg: self.cfg.clone(), memory: eve_memory::Memory::new(self.cfg.clone(), self.transcripts(), None), index }
    }
}

fn run_store(seams: &Seams, argv: &[&str], answer: &str) -> (i32, Captured) {
    let mut words = vec!["memory"];
    words.extend_from_slice(argv);
    let (io, seen) = capture(no_post(), Some(answer.to_string()));
    let rc = match parse(&full_parser(&no_env), &s(&words), &io) {
        Ok(args) => run_memory_store(&args, &io, seams),
        Err(code) => code,
    };
    (rc, Captured::take(&seen))
}

fn seams_over(world: Arc<MemoryWorld>, index: Option<Arc<dyn StoreIndex>>) -> Seams {
    Seams {
        open: Box::new(move |_| Some(world.store(index.clone()))),
        make_client: Box::new(|| None),
        dreamer: memory::real_dreamer(),
    }
}

#[test]
fn test_forget_previews_asks_and_forgets_only_on_yes() {
    let world = Arc::new(MemoryWorld::new());
    let t = world.transcripts();
    assert!(t.append("telegram", &t.new_conv("telegram"), "owner", "say", SECRET));
    let seams = seams_over(world.clone(), None);

    let (rc, out) = run_store(&seams, &["forget", "blue pot"], "n");
    assert_eq!(rc, 0);
    assert!(out.out.contains("1 matching lines") && out.out.contains("nothing changed") && !out.out.contains(SECRET));
    assert!(!world.transcripts().find("blue pot").is_empty()); // a no changes nothing

    let (rc, out) = run_store(&seams, &["forget", "blue pot"], "y");
    assert_eq!(rc, 0);
    assert!(out.out.contains("lines forgotten") && !out.out.contains(SECRET), "{}", out.out);
    assert!(world.transcripts().find("blue pot").is_empty());

    let (rc, out) = run_store(&seams, &["forget", "blue pot"], "");
    assert_eq!(rc, 0);
    assert!(out.out.contains("nothing in memory matches"));
}

#[test]
fn test_forget_with_memory_off_says_so() {
    let env: HashMap<String, String> = HashMap::new();
    let seams = Seams {
        open: Box::new(move |io| memory::open_memory_store(Some(&env), io)),
        make_client: Box::new(|| None),
        dreamer: memory::real_dreamer(),
    };
    let (rc, out) = run_store(&seams, &["forget", "anything"], "");
    assert_eq!(rc, 1);
    assert!(out.err.contains("memory: off"));
}

#[test]
fn test_the_store_must_be_set_up_by_the_daemon_first() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("memory");
    let env: HashMap<String, String> = [
        ("EVE_MEMORY".to_string(), "1".to_string()),
        ("EVE_MEMORY_DIR".to_string(), dir.to_string_lossy().into_owned()),
        ("EVE_CHAT_LOG".to_string(), "0".to_string()),
        ("EVE_MEM0".to_string(), "0".to_string()),
    ]
    .into();
    let (io, seen) = capture(no_post(), None);
    assert!(memory::open_memory_store(Some(&env), &io).is_none());
    assert!(Captured::take(&seen).err.contains("not set up yet"));
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join(eve_memory::MIGRATED_FILE), "{}").unwrap();
    let opened = memory::open_memory_store(Some(&env), &io).expect("the control: set up, it opens");
    assert_eq!(opened.cfg.store, dir);
}

#[test]
fn test_the_cli_forget_reaches_the_chat_log() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("memory");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(dir.join(eve_memory::MIGRATED_FILE), "{}").unwrap();
    let mut env: HashMap<String, String> = [
        ("EVE_MEMORY".to_string(), "1".to_string()),
        ("EVE_MEMORY_DIR".to_string(), dir.to_string_lossy().into_owned()),
        ("EVE_CHAT_LOG_FILE".to_string(), tmp.path().join("chat/log.json").to_string_lossy().into_owned()),
        ("EVE_MEM0".to_string(), "0".to_string()),
    ]
    .into();
    let (io, _) = capture(no_post(), None);
    let opened = memory::open_memory_store(Some(&env), &io).unwrap();
    assert!(opened.memory.chat_log().is_some());
    // control: with the log off, there is none to forget in
    env.insert("EVE_CHAT_LOG".into(), "0".into());
    let opened = memory::open_memory_store(Some(&env), &io).unwrap();
    assert!(opened.memory.chat_log().is_none());
}

struct FakeDreamer {
    days: Arc<Mutex<Vec<String>>>,
    changed: usize,
}

impl NightDreamer for FakeDreamer {
    fn due_nights(&self, _now: chrono::DateTime<chrono::Local>) -> Vec<String> {
        vec!["2026-09-21".into()]
    }
    fn dream(&self, day: &str) -> eve_dream::DreamReport {
        self.days.lock().unwrap().push(day.to_string());
        let mut r = eve_dream::DreamReport::new(day);
        r.dreamt = true;
        r.changed = self.changed;
        r
    }
}

struct NoClient;

impl eve_records::DreamClient for NoClient {
    fn reconcile(&self, _body: &str) -> anyhow::Result<String> {
        anyhow::bail!("no model in a test")
    }
    fn consolidate(&self, _body: &str) -> anyhow::Result<String> {
        anyhow::bail!("no model in a test")
    }
}

fn dream_seams(world: Arc<MemoryWorld>, days: Arc<Mutex<Vec<String>>>, changed: usize, client: bool) -> Seams {
    Seams {
        open: Box::new(move |_| Some(world.store(None))),
        make_client: Box::new(move || client.then(|| Arc::new(NoClient) as memory::Client)),
        dreamer: Box::new(move |_, _, _, _| Box::new(FakeDreamer { days: days.clone(), changed })),
    }
}

#[test]
fn test_dream_one_day() {
    let world = Arc::new(MemoryWorld::new());
    let days: Arc<Mutex<Vec<String>>> = Arc::default();
    let seams = dream_seams(world.clone(), days.clone(), 2, true);
    let (rc1, out1) = run_store(&seams, &["dream", "--day", "2026-09-20"], "");
    let (rc2, out2) = run_store(&seams, &["dream"], "");
    assert_eq!((rc1, rc2), (0, 0));
    assert_eq!(*days.lock().unwrap(), s(&["2026-09-20", "2026-09-21"]));
    assert!(format!("{}{}", out1.out, out2.out).contains("2 records changed"));
    assert_eq!(run_store(&seams, &["dream", "--day", "yesterday"], "").0, 2);
    let seams = dream_seams(world, days, 2, false);
    let (rc, out) = run_store(&seams, &["dream"], "");
    assert_eq!(rc, 1);
    assert!(out.err.contains("no model client"));
}

#[test]
fn test_dream_day_refuses_the_day_that_is_not_over() {
    let world = Arc::new(MemoryWorld::new());
    let days: Arc<Mutex<Vec<String>>> = Arc::default();
    let seams = dream_seams(world.clone(), days.clone(), 1, true);
    let current = world.transcripts().day_of(chrono::Local::now().fixed_offset());
    let day = chrono::NaiveDate::parse_from_str(&current, "%Y-%m-%d").unwrap();
    let later = (day + chrono::Duration::days(3)).format("%Y-%m-%d").to_string();
    for d in [current.clone(), later] {
        let (rc, out) = run_store(&seams, &["dream", "--day", &d], "");
        assert_eq!(rc, 2);
        assert!(out.err.contains("not over"));
    }
    assert!(days.lock().unwrap().is_empty());
    let earlier = (day - chrono::Duration::days(1)).format("%Y-%m-%d").to_string();
    assert_eq!(run_store(&seams, &["dream", "--day", &earlier], "").0, 0); // positive control
    assert_eq!(*days.lock().unwrap(), vec![earlier]);
}

struct FakeIndex {
    calls: Mutex<Vec<String>>,
}

impl eve_dream::DreamIndex for FakeIndex {
    fn ingest_day(&self, _t: &eve_transcripts::Transcripts, _day: &str) -> anyhow::Result<usize> {
        Ok(0)
    }
    fn tidy(&self) -> anyhow::Result<usize> {
        Ok(0)
    }
}

impl StoreIndex for FakeIndex {
    fn home(&self) -> Option<PathBuf> {
        None
    }
    fn ingest_day(&self, _transcripts: &eve_transcripts::Transcripts, day: &str) -> usize {
        self.calls.lock().unwrap().push(day.to_string());
        3
    }
    fn tidy(&self) -> usize {
        1
    }
    fn is_open(&self) -> bool {
        true
    }
    fn as_dream(&self) -> Arc<dyn eve_dream::DreamIndex> {
        Arc::new(FakeIndex { calls: Mutex::new(Vec::new()) })
    }
}

#[test]
fn test_reindex_clears_the_ledger_and_reads_every_day() {
    let world = Arc::new(MemoryWorld::new());
    let t = world.transcripts();
    for day in ["2026-09-20", "2026-09-21"] {
        let naive = chrono::NaiveDateTime::parse_from_str(&format!("{day} 12:00"), "%Y-%m-%d %H:%M").unwrap();
        use chrono::TimeZone;
        let when = chrono::Local.from_local_datetime(&naive).single().unwrap().fixed_offset();
        let opts = eve_transcripts::Append { now: Some(when), ..Default::default() };
        assert!(t.append_with("voice", &t.new_conv("voice"), "owner", "say", "hello", opts));
    }
    let ledger = world.cfg.mem0_dir().join("ingested_convs");
    std::fs::create_dir_all(ledger.parent().unwrap()).unwrap();
    std::fs::write(&ledger, "v-1\n").unwrap();
    let index = Arc::new(FakeIndex { calls: Mutex::new(Vec::new()) });
    let seams = seams_over(world.clone(), Some(index.clone()));
    let (rc, out) = run_store(&seams, &["reindex"], "");
    assert_eq!(rc, 0, "{}", out.err);
    assert_eq!(*index.calls.lock().unwrap(), s(&["2026-09-20", "2026-09-21"]));
    assert!(!ledger.exists());
    assert!(out.out.contains("2 days read, 6 memories added, 1 duplicates removed"));
    let other = Arc::new(MemoryWorld::new());
    assert_eq!(run_store(&seams_over(other, None), &["reindex"], "").0, 1); // the index is off
}

#[test]
fn test_memory_bus_lines() {
    assert_eq!(memory::tail_topics("/eve/x"), s(&["/eve/x"]));
    let all = memory::tail_topics("/eve/*");
    assert!(!all.is_empty() && all.iter().all(|t| t.starts_with("/eve/")));
    assert_eq!(memory::tail_topics("/nowhere/*"), s(&["/nowhere/*"]));
    let line = memory::tail_line(r#"{"op":"publish","topic":"/eve/a","msg":{"time":0,"x":"é"}}"#).unwrap();
    assert!(line.ends_with(r#" /eve/a {"time": 0, "x": "é"}"#), "{line}");
    assert!(memory::tail_line(r#"{"op":"status"}"#).is_none() && memory::tail_line("not json").is_none());
    let (io, seen) = capture(no_post(), None);
    let msg = json!({"op": "service_response", "id": "recall-1", "values": {"results": [
        {"id": 3, "time": "09:00", "title": "lunch", "text": "noodles"}, {"title": ""}]}});
    assert_eq!(memory::recall_answer(&msg, &io), Some(0));
    assert_eq!(Captured::take(&seen).out, "#3 09:00 lunch\n    noodles\n#?\n");
    assert_eq!(memory::recall_answer(&json!({"op": "service_response", "id": "other"}), &io), None);
    let refused = json!({"op": "service_response", "id": "recall-1", "result": false, "values": {}});
    assert_eq!(memory::recall_answer(&refused, &io), Some(1));
    assert_eq!(Captured::take(&seen).err, "recall: the service refused\n");
}

// ---- full eve's commands: diag, mic, lesson, link, notify -----------------------------------------------------

fn diag(d: Option<Value>, connected: bool) -> String {
    let (io, seen) = capture(no_post(), None);
    render_diag(d.as_ref(), connected, &io);
    Captured::take(&seen).out
}

#[test]
fn test_abnormal_reset_is_flagged() {
    let out = diag(
        Some(json!({"boot": 7, "reset": "TASK-WATCHDOG", "up": 42, "heap": 1000, "minheap": 900, "psram": 8000,
                    "last": ["1200:voice start", "1500:gesture tap"]})),
        true,
    );
    assert!(out.contains("ABNORMAL") && out.contains("TASK-WATCHDOG") && out.contains("voice start"));
    assert!(out.contains("1.20s") && out.contains("gesture tap")); // ms stamps rendered as seconds
}

#[test]
fn test_clean_poweron_is_not_flagged() {
    let out = diag(
        Some(json!({"boot": 1, "reset": "power-on", "up": 3, "heap": 1000, "minheap": 900, "psram": 8000, "last": []})),
        true,
    );
    assert!(!out.contains("ABNORMAL") && out.contains("no surviving event ring"));
}

#[test]
fn test_every_crash_class_is_flagged() {
    for reason in ["PANIC", "INT-WATCHDOG", "BROWNOUT", "other-watchdog"] {
        let out = diag(
            Some(json!({"boot": 2, "reset": reason, "up": 1, "heap": 1, "minheap": 1, "psram": 1, "last": []})),
            true,
        );
        assert!(out.contains("ABNORMAL"), "{reason}");
    }
}

#[test]
fn test_no_report_yet_explains_itself() {
    let out = diag(None, false);
    assert!(out.contains("no diag report yet") && out.contains("board connected: False"));
}

#[test]
fn test_malformed_timestamp_does_not_crash() {
    let out = diag(
        Some(json!({"boot": 1, "reset": "software", "up": 1, "heap": 1, "minheap": 1, "psram": 1,
                    "last": ["notanumber:weird"]})),
        true,
    );
    assert!(out.contains("weird"));
}

#[test]
fn test_describe_mic() {
    let base = json!({"ok": true, "mic": "on", "listening": false, "connected": false, "always": false, "available": true});
    let with = |k: &str, v: Value| {
        let mut b = base.clone();
        b[k] = v;
        b
    };
    assert!(describe_mic(&with("available", json!(false))).starts_with("eve has no microphone"));
    assert!(describe_mic(&with("mic", json!("off"))).starts_with("microphone off (your choice)"));
    assert_eq!(describe_mic(&with("listening", json!(true))), "microphone on: listening for the wake word");
    let mut always = with("listening", json!(true));
    always["always"] = json!(true);
    assert!(describe_mic(&always).contains("EVE_MIC_ALWAYS"));
    assert_eq!(describe_mic(&base), "microphone closed: it opens when the robot connects");
    assert!(describe_mic(&with("connected", json!(true))).contains("could not be opened"));
}

type Sent = Arc<Mutex<Vec<(Value, f64)>>>;

/// The daemon's socket as the lesson tests see it: every event kept with its timeout, `reply` answered.
fn fake_post(reply: Option<Value>) -> (crate::io::Post, Sent) {
    let sent: Sent = Arc::default();
    let s2 = sent.clone();
    let post: crate::io::Post = Box::new(move |event, _, timeout, _| {
        s2.lock().unwrap().push((event.clone(), timeout));
        reply.clone()
    });
    (post, sent)
}

fn lesson(argv: &[&str], reply: Option<Value>) -> (i32, Captured, Vec<(Value, f64)>) {
    let (post, sent) = fake_post(reply);
    let (io, seen) = capture(post, None);
    let mut words = vec!["lesson"];
    words.extend_from_slice(argv);
    let rc = full_main(&s(&words), &io, &no_env);
    let sent = sent.lock().unwrap().clone();
    (rc, Captured::take(&seen), sent)
}

#[test]
fn test_help_lists_every_action_and_option() {
    let (rc, out, _) = lesson(&["--help"], None);
    assert_eq!(rc, 0);
    for word in eve_learning::LESSON_ACTIONS.iter().copied().chain(["--mode", "--topic", "--level", "--text"]) {
        assert!(out.out.contains(word), "{word}");
    }
}

#[test]
fn test_start_sends_the_lesson_event_and_prints_the_answer() {
    let reply = json!({"ok": true, "action": "start", "answer": "What is 3/4 + 1/4?"});
    let (rc, out, sent) = lesson(&["start", "--mode", "learn", "--topic", "Fractions", "--level", "Grade 4"], Some(reply));
    assert_eq!(rc, 0);
    assert_eq!(sent[0].0, json!({"evt": "lesson", "action": "start", "mode": "learn", "topic": "Fractions", "level": "Grade 4"}));
    assert!(sent[0].1 >= 120.0);
    assert!(out.out.contains("What is 3/4 + 1/4?"));
}

#[test]
fn test_quick_actions_use_a_short_timeout() {
    let (rc, _, sent) = lesson(&["open"], Some(json!({"ok": true})));
    assert_eq!(rc, 0);
    assert_eq!(sent[0].0, json!({"evt": "lesson", "action": "open"}));
    assert!(sent[0].1 < 60.0);
}

#[test]
fn test_daemon_not_running_exits_2() {
    let (rc, out, _) = lesson(&["hint"], None);
    assert_eq!(rc, 2);
    assert!(out.err.contains("daemon not reachable"));
}

#[test]
fn test_a_refusal_exits_1_with_the_reason() {
    let (rc, out, _) = lesson(&["check"], Some(json!({"ok": false, "error": "Open a lesson first."})));
    assert_eq!(rc, 1);
    assert!(out.err.contains("Open a lesson first."));
}

#[test]
fn test_an_invalid_action_is_rejected_by_argparse() {
    let (rc, _, sent) = lesson(&["dance"], Some(json!({"ok": true})));
    assert_eq!(rc, 2);
    assert!(sent.is_empty());
}

#[test]
fn test_bare_ideas_is_refused_so_saved_thinking_is_not_erased() {
    let (rc, out, sent) = lesson(&["ideas"], Some(json!({"ok": true})));
    assert_eq!(rc, 2);
    assert!(sent.is_empty() && out.err.contains("--text"));
}

#[test]
fn test_empty_ideas_text_is_still_sent() {
    let (rc, _, sent) = lesson(&["ideas", "--text", ""], Some(json!({"ok": true})));
    assert_eq!(rc, 0);
    assert_eq!(sent[0].0, json!({"evt": "lesson", "action": "ideas", "text": ""}));
}

#[test]
fn test_status_prints_the_lesson() {
    let reply = json!({"ok": true, "action": "status", "answer": "Nice work.",
                       "lesson": {"topic": "Fractions", "mode": "learn", "stage": "practice", "problem": "1/2 + 1/4"}});
    let (rc, out, _) = lesson(&["status"], Some(reply));
    assert_eq!(rc, 0);
    assert!(out.out.contains("Fractions (learn, practice): 1/2 + 1/4") && out.out.contains("Nice work."));
}

#[test]
fn test_listen_commands_send_the_lesson_event_with_a_short_timeout() {
    for action in ["listen", "stop-listening"] {
        let (rc, out, sent) = lesson(&[action], Some(json!({"ok": true, "action": action, "answer": "I'm listening."})));
        assert_eq!(rc, 0);
        assert_eq!(sent[0].0, json!({"evt": "lesson", "action": action}));
        assert!(sent[0].1 < 60.0);
        assert!(out.out.contains("I'm listening."));
    }
}

#[test]
fn test_link_pair_prints_the_pairing_link() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("stick-link.json");
    eve_stick_link::LinkToken::new(&path).ensure().unwrap();
    let (io, seen) = capture(no_post(), None);
    let rc = run_link_with(&io, &path, &|_, _| "https://new-one.trycloudflare.com".to_string(), &|_| None);
    assert_eq!(rc, 0);
    let out = Captured::take(&seen).out;
    assert!(out.contains(&format!("https://new-one.trycloudflare.com{}#t=", eve_stick_link::PAIR_PATH)), "{out}");
    assert!(out.contains("brew install qrencode"));
}

#[test]
fn test_link_pair_says_so_when_no_address_is_posted() {
    let tmp = tempfile::tempdir().unwrap();
    let (io, seen) = capture(no_post(), None);
    let rc = run_link_with(&io, &tmp.path().join("stick-link.json"), &|_, _| String::new(), &|_| None);
    assert_eq!(rc, 1);
    assert!(Captured::take(&seen).out.contains("isn't posted yet"));
}

const TOKEN: &str = "test-link-token-0123456789abcdef";

#[test]
fn test_cli_topic_and_test() {
    let calls: Arc<Mutex<Vec<(String, Vec<u8>)>>> = Arc::default();
    let c2 = calls.clone();
    let post: eve_brain::notify::PostFn = Arc::new(move |url, body, _headers| {
        c2.lock().unwrap().push((url.to_string(), body.to_vec()));
        Ok(200)
    });
    let cfg = eve_brain::notify::NotifyConfig { enabled: true, base: eve_brain::notify::NOTIFY_URL.into(), preview: false };
    let (io, seen) = capture(no_post(), None);
    assert_eq!(run_notify_with("topic", &io, &cfg, TOKEN, &post), 0);
    let out = Captured::take(&seen).out;
    let topic = eve_brain::notify::notify_topic(TOKEN);
    assert!(out.contains(&topic) && out.contains(&format!("https://ntfy.sh/{topic}")) && out.contains("App Store"));
    assert!(!out.contains(TOKEN));
    assert_eq!(run_notify_with("test", &io, &cfg, TOKEN, &post), 0);
    let calls = calls.lock().unwrap();
    assert!(!calls.is_empty() && calls[0].1 == eve_brain::notify::TEST_LINE.as_bytes());
}

#[test]
fn test_cli_without_a_pairing() {
    let post: eve_brain::notify::PostFn = Arc::new(|_, _, _| panic!("posted without a pairing"));
    let (io, seen) = capture(no_post(), None);
    assert_eq!(run_notify_with("topic", &io, &eve_brain::notify::NotifyConfig::default(), "", &post), 1);
    assert!(Captured::take(&seen).out.contains("not paired"));
}

#[test]
fn test_species_names_and_indexes() {
    let (post, sent) = fake_post(Some(json!({"connected": true})));
    let (io, seen) = capture(post, None);
    assert_eq!(full_main(&s(&["species", " Capybara "]), &io, &no_env), 0);
    assert_eq!(full_main(&s(&["species", "gif"]), &io, &no_env), 0);
    assert_eq!(full_main(&s(&["species", "17"]), &io, &no_env), 0);
    assert_eq!(full_main(&s(&["species", "18"]), &io, &no_env), 2);
    assert_eq!(full_main(&s(&["species", "dog"]), &io, &no_env), 2);
    let idx: Vec<Value> = sent.lock().unwrap().iter().map(|(e, _)| e["idx"].clone()).collect();
    assert_eq!(idx, vec![json!(0), json!(255), json!(17)]);
    let out = Captured::take(&seen);
    assert!(out.out.contains("species set to capybara (index 0)") && out.out.contains("species set to GIF pack"));
    assert!(out.err.contains("index 18 out of range (0..17)") && out.err.contains("unknown species 'dog'"));
}

#[test]
fn test_move_builds_each_kind_of_request() {
    let (post, sent) = fake_post(Some(json!({"ok": true, "sent": {}, "asked": {}})));
    let (io, _) = capture(post, None);
    let _ = full_main(&s(&["move", "--stop"]), &io, &no_env);
    let _ = full_main(&s(&["move", "sway", "--yaw-amp", "-10", "--speed", "1.5"]), &io, &no_env);
    let _ = full_main(&s(&["move", "--keys", "[[0,127,35,400]]"]), &io, &no_env);
    let sent = sent.lock().unwrap();
    assert_eq!(sent[0].0, json!({"evt": "move", "kind": "stop"}));
    assert_eq!(sent[1].0["kind"], json!("osc"));
    assert_eq!(sent[1].0["yaw_amp"], json!(-10));
    assert_eq!(sent[1].0["speed"], json!(1.5));
    assert_eq!(sent[2].0, json!({"evt": "move", "kind": "keys", "keys": [[0, 127, 35, 400]]}));
}

#[test]
fn test_the_cli_runs_the_server_before_reading_the_env_file() {
    // `mcp` and `vault-mcp` are taken before the env file and the profile (main's first lines): neither parser
    // knows them as a command
    assert!(!full_parser(&no_env).command_names().contains(&"mcp".to_string()));
    assert!(!lite::lite_parser().command_names().contains(&"mcp".to_string()));
}

#[test]
fn test_app_help_is_argparse_shaped() {
    let help = crate::apphelp::help_for("lights", &s(&["--help"])).unwrap();
    assert!(help.starts_with("usage: eve-bridge lights [-h] {scan,list,name,wiz-import,set} ..."), "{help}");
    let help = crate::apphelp::help_for("obsbot", &s(&["--json", "raw", "-h"])).unwrap();
    assert!(help.starts_with("usage: eve-bridge obsbot raw [-h] address [args ...]"), "{help}");
    assert!(crate::apphelp::help_for("lights", &s(&["list"])).is_none());
    assert!(crate::apphelp::help_for("lights", &s(&["bogus", "--help"])).is_none()); // the app says it is bogus
    assert!(crate::apphelp::help_for("spotify", &s(&["play", "--", "--help"])).is_none());
}

// ---- web open and signals: in a child process, since a signal ends the process it is sent to ------------------

/// Run `child` in a forked copy of this process and return how it ended (its exit status; -1 for a signal). The
/// child never returns to the test runner: it `_exit`s with what `child` returns, or 97 on a panic.
fn in_child(child: impl FnOnce() -> i32) -> i32 {
    // SAFETY: the child calls only what the test gives it and then _exit; it never unwinds into the runner.
    let pid = unsafe { libc::fork() };
    assert!(pid >= 0, "fork failed");
    if pid == 0 {
        let code = std::panic::catch_unwind(std::panic::AssertUnwindSafe(child)).unwrap_or(97);
        // SAFETY: ends the child without running the runner's exit handlers.
        unsafe { libc::_exit(code) };
    }
    let mut status: libc::c_int = 0;
    // SAFETY: waits for the child forked above.
    let waited = unsafe { libc::waitpid(pid, &mut status, 0) };
    assert_eq!(waited, pid);
    if libc::WIFEXITED(status) {
        libc::WEXITSTATUS(status)
    } else {
        -1
    }
}

/// The handler installed for `sig` now.
fn handler_of(sig: libc::c_int) -> libc::sighandler_t {
    // SAFETY: a query only (no new action).
    unsafe {
        let mut old: libc::sigaction = std::mem::zeroed();
        libc::sigaction(sig, std::ptr::null(), &mut old);
        old.sa_sigaction
    }
}

#[test]
fn test_sigterm_and_sighup_end_web_open_through_its_cleanup() {
    for sig in [libc::SIGTERM, libc::SIGHUP] {
        let tmp = tempfile::tempdir().unwrap();
        let page = tmp.path().join("eve-open-x.html");
        std::fs::write(&page, "page").unwrap();
        let p = page.clone();
        let code = in_child(move || {
            // the handlers come back as they were when the guard goes
            let before = handler_of(sig);
            drop(lite::StopOnSignals::new(&p));
            if handler_of(sig) != before {
                return 98;
            }
            let _guard = lite::StopOnSignals::new(&p);
            // SAFETY: signals this child only.
            unsafe { libc::kill(libc::getpid(), sig) };
            std::thread::sleep(Duration::from_secs(5)); // the handler ends the process before this returns
            99
        });
        assert_eq!(code, 128 + sig, "signal {sig}");
        assert!(!page.exists(), "signal {sig} left the page");
    }
}

#[test]
fn test_web_open_deletes_the_page_on_ctrl_c() {
    let w = World::new();
    let opened = w.root.join("opened");
    let mut ctx = w.ctx();
    ctx.catch_signals = true;
    let marker = opened.clone();
    ctx.open_in_browser = Some(Box::new(move |path| {
        std::fs::write(&marker, path).unwrap();
        true
    }));
    let code = in_child(move || {
        // the daemon hands out a code; while web open waits for the browser to use it, Ctrl-C
        let post: crate::io::Post = Box::new(|event, _, _, _| match event["evt"].as_str() {
            Some("web_login") => Some(json!({"ok": true, "code": CANARY, "port": PORT})),
            _ => {
                // SAFETY: signals this child only.
                unsafe { libc::kill(libc::getpid(), libc::SIGINT) };
                std::thread::sleep(Duration::from_secs(5));
                Some(json!({"ok": true, "used": false}))
            }
        });
        let (io, _) = capture(post, None);
        lite::lite_main(&s(&["web", "open", "--wait", "1"]), &[], &ctx, &io)
    });
    assert_eq!(code, 128 + libc::SIGINT);
    let page = std::fs::read_to_string(&opened).expect("the browser was given the page");
    assert!(!Path::new(&page).exists());
    assert!(w.pages().is_empty(), "{:?}", w.pages());
}

#[test]
fn test_web_open_through_lite_daemons_own_ipc() {
    // End to end with web_auth's MacLogin answering as lite.py's handler does (`web_login`: a code it issued and the
    // page's port; `web_login_used`: whether a browser consumed it): the code issued is the one the page carries,
    // the browser's sign-in consumes it, `web_login_used` then says so, and the code is never printed. The whole
    // LiteDaemon is not started here: its page server is private to eve-lite, whose own tests run this IPC.
    let w = World::new();
    let login = Arc::new(eve_web_auth::MacLogin::new());
    let handler_login = login.clone();
    let handler: Arc<dyn Fn(&Value) -> Value + Send + Sync> = Arc::new(move |req| {
        match req.get("evt").and_then(Value::as_str).unwrap_or("") {
            "web_login" => json!({"ok": true, "code": handler_login.issue(), "port": PORT}),
            "web_login_used" => json!({"ok": true, "used": handler_login.used(req.get("code").and_then(Value::as_str))}),
            _ => json!({"ok": false, "error": eve_lite::UNKNOWN_EVENT}),
        }
    });
    let _serving = Serving::new(&w.sock, handler);
    let codes: Arc<Mutex<Vec<String>>> = Arc::default();
    let mut ctx = w.ctx();
    let (seen, browser_login) = (codes.clone(), login.clone());
    ctx.open_in_browser = Some(Box::new(move |path| {
        let page = std::fs::read_to_string(path).unwrap();
        let code = page.split_once("#login=").unwrap().1.split('"').next().unwrap().to_string();
        assert!(browser_login.consume(Some(&code))); // the page's POST to /api/mac/login
        seen.lock().unwrap().push(code);
        true
    }));
    let (rc, out) = run_lite(&["web", "open", "--wait", "5"], &ctx);
    assert_eq!(rc, 0, "{}", out.err);
    let code = codes.lock().unwrap()[0].clone();
    assert!(eve_web_auth::TOKEN.is_match(&code) && login.used(Some(&code)));
    assert!(!login.consume(Some(&code))); // single use: the browser had it
    assert_eq!(out.out, format!("eve is open in your browser: http://localhost:{PORT}\n"));
    assert!(!out.out.contains(&code) && !out.err.contains(&code));
    assert!(w.pages().is_empty());
}
