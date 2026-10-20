//! eve lite's command line (EVE_PROFILE=lite): `eve-bridge [daemon|install|uninstall|status|update|web|doctor|lite|
//! memory]`. cli.py's lite section, from `EX_CONFIG` to `_lite_uninstall`.
//!
//! Every command reads what it needs from a [`Ctx`]: the environment profile.apply left, HOME, launchd, the
//! programs it starts and the browser. The binary's is [`Ctx::process`]; tests lend their own, so they can run side
//! by side in one process.

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use regex::Regex;
use serde_json::{json, Value};

use eve_service::service_launchd::{LABEL, LITE_LABEL};
use eve_service::{Host, Launch, Launchd, SystemHost};

use super::memory::{run_memory_store, Seams};
use super::{parse, print_help};
use crate::argparse::{Arg, Parser, Val, SUPPRESS};
use crate::io::Io;
use crate::py::{self, get, pstr};

/// sysexits.h: EVE_PROFILE is neither full nor lite (profile.ProfileError).
pub const EX_CONFIG: i32 = 78;
pub const LITE_INSTALL_LINE: &str =
    "curl -fsSL https://github.com/gurul/eve_01/releases/latest/download/install-lite.sh | bash";
/// install-lite.sh's marker: the env file's SHA-256, when an uninstall kept the folder.
pub const LITE_KEPT: &str = ".eve-lite-kept";
/// `web open` waits this long for eve to answer; a restart after a sign-in takes ~5 s.
pub const WEB_OPEN_WAIT_SECS: f64 = 20.0;
pub const IPC_POLL_SECS: f64 = 0.5;
pub const OPEN: &str = "/usr/bin/open";
pub const OSADECOMPILE: &str = "/usr/bin/osadecompile";
pub const LAUNCH_SERVICES: &str = "Library/Preferences/com.apple.LaunchServices/com.apple.launchservices.secure.plist";
/// `<unistd.h>`: confstr's name for this user's own temporary folder.
pub const CS_DARWIN_USER_TEMP_DIR: i32 = 65537;
static BUNDLE_ID: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9.-]{0,254}\z").unwrap());
/// A run this long is a key or a code, never a word (lite_web).
static OPAQUE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[A-Za-z0-9_\-+/=.%]{24,}").unwrap());
/// doctor's marks: fine; nothing to do (or optional); to fix.
pub const OK: &str = "ok";
pub const INFO: &str = "--";
pub const FIX: &str = "!!";

// What eve lite's commands say: one plain line each.
pub const LITE_NOT_ANSWERING: &str = "eve is not answering on this Mac. `eve-bridge status` says whether its login \
service is installed and loaded; its log is ~/Library/Logs/eve-lite.log.";
pub const LITE_REFUSED: &str = "eve did not do that. Try again.";
pub const LITE_NO_HOOKS: &str = "eve lite does not use Claude Code's hooks, so there is nothing to install there. \
`eve-bridge install --service` installs eve lite's login service.";
pub const LITE_NO_HOOKS_GONE: &str = "eve lite installs no Claude Code hooks, so there is nothing to remove there. \
`eve-bridge lite uninstall` removes eve lite.";
pub static LITE_UPDATE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "eve-bridge: eve lite does not update itself. Run its install line again; it keeps your settings and \
         memory:\n  {LITE_INSTALL_LINE}"
    )
});
pub const LITE_MAC_ONLY: &str = "eve-bridge: eve lite runs on a Mac (macOS 14 or later), not on {platform!r}.";
pub const LITE_NOT_LOADED: &str = "eve-bridge: eve lite is not running on this Mac, and its login service is not \
loaded. `eve-bridge install --service` starts it.";
pub const LITE_NO_CODE: &str = "eve-bridge: eve answered without a sign-in code. Try again.";
pub const LITE_NO_BROWSER: &str = "eve-bridge: no browser opened. Run this in a Terminal on the Mac itself, signed \
in at its screen, or open Eve from Applications.";
pub const LITE_LOGIN_UNUSED: &str = "eve-bridge: the browser did not sign in to eve within two minutes, so that \
sign-in is gone. Run `eve-bridge web open` again, or open Eve from Applications.";

/// A program's run as the lite commands see it: its exit code and stdout. Err where it could not start or ran out
/// of time (Python's OSError or SubprocessError).
pub type Run = Box<dyn Fn(&[String], Duration) -> std::io::Result<(i32, String)> + Send + Sync>;

/// What eve lite's commands read and start, in one place.
pub struct Ctx {
    /// The environment as profile.apply left it.
    pub env: HashMap<String, String>,
    /// `Path.home()`.
    pub home: PathBuf,
    /// launchd, for eve lite's login service (its host runs launchctl).
    pub launchd: Launchd,
    /// `sys.platform`.
    pub platform: String,
    /// `subprocess.run` for open and osadecompile.
    pub run: Run,
    /// `_open_in_browser`, replaced (tests); None: the real one, through `run`.
    pub open_in_browser: Option<Box<dyn Fn(&str) -> bool + Send + Sync>>,
    pub poll_secs: f64,
    /// `web_auth.LOGIN_SECS`.
    pub login_secs: f64,
    /// `lite.run_daemon`.
    pub run_daemon: Box<dyn Fn(&eve_lite::DaemonArgs) -> i32 + Send + Sync>,
    /// `lite.setup_logging`.
    pub setup_logging: Box<dyn Fn(&str) + Send + Sync>,
    /// The memory folder's commands (`memory forget`).
    pub memory: Seams,
    /// Whether SIGTERM, SIGHUP and SIGINT are caught while `web open`'s page exists (the binary: yes; tests: no).
    pub catch_signals: bool,
}

/// `sys.platform`.
pub fn sys_platform() -> String {
    if cfg!(target_os = "macos") {
        "darwin".into()
    } else if cfg!(target_os = "linux") {
        "linux".into()
    } else if cfg!(windows) {
        "win32".into()
    } else {
        std::env::consts::OS.into()
    }
}

/// `subprocess.run(argv, capture_output=True, timeout=..., stdin=DEVNULL)`: exit code and stdout.
pub fn run_program(argv: &[String], timeout: Duration) -> std::io::Result<(i32, String)> {
    use std::io::Read;
    let (program, rest) = argv.split_first().ok_or_else(|| std::io::Error::other("no program"))?;
    let mut child = std::process::Command::new(program)
        .args(rest)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()?;
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        if let Some(s) = stdout.as_mut() {
            let _ = s.read_to_end(&mut out);
        }
        out
    });
    let end = Instant::now() + timeout;
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= end {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "TimeoutExpired"));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let out = reader.join().unwrap_or_default();
    Ok((status.code().unwrap_or(-1), String::from_utf8_lossy(&out).into_owned()))
}

/// `Path.home()` for this process.
pub fn process_home() -> PathBuf {
    eve_envfile::expanduser(Path::new("~")).unwrap_or_else(|_| PathBuf::from("/"))
}

/// launchd as eve lite's commands use it, over `home` and `host`.
pub fn launchd_for(home: &Path, host: Arc<dyn Host>) -> Launchd {
    Launchd {
        env: eve_claude_home::HomeEnv {
            claude_config_dir: None,
            claude_config_dirs: None,
            home: Some(home.as_os_str().to_owned()),
        },
        full: eve_service::FullPaths::under(home),
        label: LABEL.to_string(),
        launch: Launch::current(),
        host,
    }
}

impl Ctx {
    /// This process: its environment (as profile.apply left it), its HOME, launchd, real programs and signals.
    pub fn process() -> Ctx {
        let home = process_home();
        let mut launchd = launchd_for(&home, Arc::new(SystemHost));
        launchd.env = eve_claude_home::HomeEnv::from_process();
        Ctx {
            env: std::env::vars().collect(),
            home,
            launchd,
            platform: sys_platform(),
            run: Box::new(run_program),
            open_in_browser: None,
            poll_secs: IPC_POLL_SECS,
            login_secs: eve_web_auth::LOGIN_SECS,
            run_daemon: Box::new(|args| eve_lite::run_daemon(Some(args), eve_lite::LiteOptions::default())),
            setup_logging: Box::new(|level| eve_lite::setup_logging(level)),
            memory: Seams::process(),
            catch_signals: true,
        }
    }

    fn var(&self, name: &str) -> &str {
        self.env.get(name).map(String::as_str).unwrap_or("")
    }

    /// `~` and `~/...` against this context's HOME.
    pub fn expand(&self, path: &str) -> PathBuf {
        if path == "~" {
            self.home.clone()
        } else if let Some(rest) = path.strip_prefix("~/") {
            self.home.join(rest)
        } else {
            PathBuf::from(path)
        }
    }

    /// The env file (envfile.env_path): EVE_ENV_FILE, else ~/.config/eve-bridge/env.
    pub fn env_file(&self) -> PathBuf {
        let named = self.var(eve_envfile::ENV_FILE_VAR);
        self.expand(if named.is_empty() { eve_envfile::DEFAULT_PATH } else { named })
    }

    /// `_tilde`: `path` with this user's home written ~, for a line on the screen.
    pub fn tilde(&self, path: &Path) -> String {
        let text = path.to_string_lossy().into_owned();
        let home = self.home.to_string_lossy().into_owned();
        let base = home.trim_end_matches('/');
        if text == home || text.starts_with(&format!("{base}/")) {
            format!("~{}", &text[home.len().min(text.len())..])
        } else {
            text
        }
    }

    /// `_lite_socket`: eve lite's IPC socket, as its daemon takes it (lite.ipc_path: a socket file, never a TCP
    /// port).
    pub fn lite_socket(&self) -> String {
        let default = eve_profile::LITE_DEFAULTS
            .get(eve_lite::SOCK_ENV)
            .cloned()
            .unwrap_or_else(|| "~/.eve-lite/ipc.sock".into());
        let from_env = self.env.get(eve_lite::SOCK_ENV).map(String::as_str);
        let chosen = eve_lite::ipc_path_from(None, from_env, &self.expand(&default));
        self.expand(&chosen.to_string_lossy()).to_string_lossy().into_owned()
    }

    fn lite_loaded(&self) -> bool {
        self.launchd.lite_loaded().unwrap_or(false)
    }
}

/// `_seconds`: --wait's value: seconds, from 0 to a day.
pub fn seconds(text: &str) -> Result<Val, String> {
    let Some(value) = crate::argparse::py_float(text) else {
        return Err(format!("not a number of seconds: {}", crate::argparse::py_repr(text)));
    };
    if !(0.0..=86400.0).contains(&value) {
        // NaN fails here too
        return Err(format!("seconds from 0 to 86400, not {}", crate::argparse::py_repr(text)));
    }
    Ok(Val::Float(value))
}

/// eve lite's commands (`_lite_parser`).
pub fn lite_parser() -> Parser {
    let prog = "eve-bridge";
    let mut parser = Parser::new(
        prog,
        Some("eve lite on this Mac: its daemon, its login service and its web app (docs/lite.md)."),
        true,
    );
    parser.version(&format!("eve-bridge {} (eve lite)", eve_meta::VERSION));
    let sub = |name: &str| Parser::new(&format!("{prog} {name}"), None, true);

    let mut daemon = sub("daemon");
    daemon
        .arg(Arg::opt(&["--socket"]).help("IPC socket path override (a socket file)"))
        .arg(Arg::opt(&["--log-level"]).default_str("INFO"));
    let mut install = sub("install");
    install.arg(
        Arg::opt(&["--service"])
            .flag()
            .help("Start eve lite now and at every login (launchd agent com.github.eve-bridge.lite)"),
    );
    let mut uninstall = sub("uninstall");
    uninstall.arg(Arg::opt(&["--service"]).flag().help("Stop eve lite and remove its launchd agent"));
    let mut update = sub("update");
    update.arg(Arg::opt(&["-y", "--yes"]).flag().help(SUPPRESS));

    let mut web = sub("web");
    let wp = web.prog.clone();
    let mut open = Parser::new(&format!("{wp} open"), None, true);
    open.arg(
        Arg::opt(&["--wait"])
            .conv(seconds)
            .default(Val::Float(WEB_OPEN_WAIT_SECS))
            .metavar(&["SECONDS"])
            .help(&format!("How long to wait for eve to answer (default {WEB_OPEN_WAIT_SECS:.0})")),
    );
    let mut revoke = Parser::new(&format!("{wp} revoke"), None, true);
    revoke.arg(Arg::pos("id").metavar(&["ID|all"]));
    let mut lan = Parser::new(&format!("{wp} lan"), None, true);
    lan.arg(Arg::pos("state").choices(&["on", "off"]));
    web.subparsers(
        "web_cmd",
        vec![
            ("open", &[], Some("Open eve in this Mac's browser, signed in (Eve in Applications runs this)"), open),
            ("devices", &[], Some("List the browsers that can open eve"), Parser::new(&format!("{wp} devices"), None, true)),
            ("revoke", &[], Some("Sign a browser out of eve: its id from `web devices`, or all"), revoke),
            ("lan", &[], Some("Turn phone access on or off: eve's door for phones on this Wi-Fi"), lan),
        ],
    );

    let mut lite = sub("lite");
    let lp = lite.prog.clone();
    let mut remove = Parser::new(&format!("{lp} uninstall"), None, true);
    remove.arg(Arg::opt(&["--dry-run"]).flag().help("Say what it would remove, and remove nothing"));
    lite.subparsers(
        "lite_cmd",
        vec![("uninstall", &[], Some("Remove eve lite from this Mac; asks before deleting its memory"), remove)],
    );

    let mut memory = sub("memory");
    let mp = memory.prog.clone();
    let mut forget = Parser::new(&format!("{mp} forget"), None, true);
    forget.arg(Arg::pos("query").help("The words to forget; a phrase in double quotes matches as written"));
    memory.subparsers(
        "memory_cmd",
        vec![("forget", &[], Some("Count what matches in every store, then forget it on a yes"), forget)],
    );

    parser.subparsers(
        "cmd",
        vec![
            ("daemon", &[], Some("Run eve lite's daemon (its login service runs this)"), daemon),
            ("install", &[], Some("With --service: install eve lite's login service"), install),
            ("uninstall", &[], Some("With --service: remove eve lite's login service"), uninstall),
            ("status", &[], Some("Whether eve lite is installed, running and signed in: yes or no for each"), sub("status")),
            ("update", &[], Some("Say how eve lite updates: by running its install line again"), update),
            ("web", &[], Some("eve's web app: open it on this Mac, and the browsers that can use it"), web),
            ("doctor", &[], Some("Check eve lite on this Mac, part by part, and say what to fix"), sub("doctor")),
            ("lite", &[], Some("eve lite itself: uninstall"), lite),
            ("memory", &[], Some("eve's memory: forget"), memory),
        ],
    );
    parser
}

/// eve lite's command line. `applied`: what profile.apply changed in this process, for the daemon's log.
pub fn lite_main(argv: &[String], applied: &[String], ctx: &Ctx, io: &Io) -> i32 {
    let parser = lite_parser();
    let args = match parse(&parser, argv, io) {
        Ok(a) => a,
        Err(code) => return code,
    };
    let Some(cmd) = args.str("cmd") else {
        print_help(&parser, io);
        return 1;
    };
    match cmd.as_str() {
        "daemon" => {
            let level = args.str("log_level").unwrap_or_else(|| "INFO".into());
            (ctx.setup_logging)(&level);
            say_profile(applied);
            (ctx.run_daemon)(&eve_lite::DaemonArgs { socket: args.str("socket"), log_level: Some(level) })
        }
        "install" | "uninstall" => {
            if !args.flag("service") {
                io.say(if cmd == "install" { LITE_NO_HOOKS } else { LITE_NO_HOOKS_GONE });
                return 0;
            }
            if ctx.platform != "darwin" {
                io.warn(&LITE_MAC_ONLY.replace("{platform!r}", &crate::argparse::py_repr(&ctx.platform)));
                return 2;
            }
            super::report(if cmd == "install" { ctx.launchd.install_lite() } else { ctx.launchd.uninstall_lite() })
        }
        "update" => {
            io.warn(&LITE_UPDATE);
            2
        }
        "status" => lite_status(ctx, io),
        "doctor" => lite_doctor(ctx, io),
        _ => {
            let sub = format!("{cmd}_cmd");
            let Some(subcmd) = args.str(&sub) else {
                // web, lite and memory take a subcommand
                if let Some(p) = parser.find(&[cmd.as_str()]) {
                    print_help(p, io);
                }
                return 1;
            };
            match cmd.as_str() {
                "web" => match subcmd.as_str() {
                    "open" => run_web_open(args.get("wait").float(), ctx, io),
                    "devices" => run_web_devices(ctx, io),
                    "revoke" => run_web_revoke(&args.str("id").unwrap_or_default(), ctx, io),
                    _ => run_web_lan(args.str("state").as_deref() == Some("on"), ctx, io),
                },
                "lite" => lite_uninstall(args.flag("dry_run"), ctx, io),
                _ => run_memory_store(&args, io, &ctx.memory),
            }
        }
    }
}

/// The names `_say_profile` reports removed: secrets profile.apply took out (not one it forced or filled).
pub fn removed_secrets(applied: &[String]) -> Vec<String> {
    applied
        .iter()
        .filter(|name| {
            eve_profile::is_secret_name(name)
                && !eve_profile::LITE_FORCED.contains_key(name.as_str())
                && !eve_profile::LITE_DEFAULTS.contains_key(name.as_str())
        })
        .cloned()
        .collect()
}

/// profile.apply ran before logging was set up, so its own line went nowhere: the daemon's log gets one now, with
/// names and a count only. A secret it removed is named, so the log says why a key in the env file does nothing.
pub fn say_profile(applied: &[String]) {
    tracing::info!(target: "eve_cli", "{}", profile_line(applied));
}

/// The line [`say_profile`] logs.
pub fn profile_line(applied: &[String]) -> String {
    let removed = removed_secrets(applied);
    let listed = if removed.is_empty() { "nothing".to_string() } else { removed.join(", ") };
    format!("profile: eve lite: {} switches set; removed {listed}", applied.len() - removed.len())
}

// ---- talking to eve lite's daemon ----------------------------------------------------------------------------

/// `_lite_ask`: one request to eve lite's daemon on its 0600 socket; None when nothing answers (or the answer is not
/// a dict).
pub fn lite_ask(sock: &str, event: &Value, timeout: f64, io: &Io) -> Option<Value> {
    (io.post)(event, Some(sock), timeout, 2.0).filter(Value::is_object)
}

/// `_lite_answered`: whether the daemon answered `ok`; otherwise why not, on stderr.
pub fn lite_answered(answer: Option<&Value>, io: &Io) -> bool {
    let Some(answer) = answer else {
        io.warn(&format!("eve-bridge: {LITE_NOT_ANSWERING}"));
        return false;
    };
    if !py::is_true(get(answer, "ok")) {
        io.warn(&format!("eve-bridge: {}", plain_line(get(answer, "error"), LITE_REFUSED)));
        return false;
    }
    true
}

fn is_control(c: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
    matches!(get_general_category(c), G::Control | G::Format | G::Surrogate | G::PrivateUse | G::Unassigned)
}

/// `_shown`: `text` without control or format characters (a terminal acts on them), at most `limit` characters.
pub fn shown(text: &Value, limit: usize) -> String {
    let value = match text {
        Value::String(s) => s.clone(),
        other => pstr(other),
    };
    shown_str(&value, limit)
}

pub fn shown_str(value: &str, limit: usize) -> String {
    value.chars().filter(|c| !is_control(*c)).take(limit).collect()
}

/// `_plain_line`: `text` as one printable line; `fallback` when there is none, or when it holds a run that reads as
/// a key or a code (lite_web's rule: nothing eve shows carries one, whatever said it).
pub fn plain_line(text: &Value, fallback: &str) -> String {
    let line = match text {
        Value::String(_) => {
            let s = shown(text, 300);
            s.split(eve_envfile::is_py_space).filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ")
        }
        _ => String::new(),
    };
    if line.is_empty() || OPAQUE.is_match(&line) {
        fallback.to_string()
    } else {
        line
    }
}

/// `_is_port`: an int (not a bool) from 1 to 65535.
pub fn is_port(value: &Value) -> bool {
    match value {
        Value::Number(n) if n.is_i64() || n.is_u64() => n.as_i64().is_some_and(|p| 0 < p && p < 65536),
        _ => false,
    }
}

// ---- web open: this Mac's browser, signed in -----------------------------------------------------------------

/// The page holding a sign-in code, for the signal handler to delete (a NUL-terminated path, or null).
static PAGE: AtomicPtr<libc::c_char> = AtomicPtr::new(std::ptr::null_mut());

extern "C" fn stop_on_signal(signum: libc::c_int) {
    let page = PAGE.load(Ordering::SeqCst);
    // SAFETY: unlink and _exit are async-signal-safe; `page` is a leaked CString or null.
    unsafe {
        if !page.is_null() {
            libc::unlink(page);
        }
        libc::_exit(128 + signum);
    }
}

/// `_stop_on_signals`: SIGTERM, SIGHUP (a closed Terminal) and Ctrl-C delete the page holding a sign-in code before
/// the process ends, whatever stops `web open`. The old handlers come back when it drops.
pub struct StopOnSignals {
    saved: Vec<(libc::c_int, libc::sighandler_t)>,
    page: *mut libc::c_char,
}

impl StopOnSignals {
    pub fn new(page: &Path) -> StopOnSignals {
        use std::os::unix::ffi::OsStrExt;
        let c = std::ffi::CString::new(page.as_os_str().as_bytes()).unwrap_or_default();
        let raw = c.into_raw();
        PAGE.store(raw, Ordering::SeqCst);
        let mut saved = Vec::new();
        for sig in [libc::SIGTERM, libc::SIGHUP, libc::SIGINT] {
            // SAFETY: installing a handler that only calls async-signal-safe functions.
            let old = unsafe { libc::signal(sig, stop_on_signal as *const () as libc::sighandler_t) };
            if old != libc::SIG_ERR {
                saved.push((sig, old));
            }
        }
        StopOnSignals { saved, page: raw }
    }
}

impl Drop for StopOnSignals {
    fn drop(&mut self) {
        for (sig, handler) in &self.saved {
            // SAFETY: putting back the handler signal() returned.
            unsafe {
                libc::signal(*sig, *handler);
            }
        }
        PAGE.store(std::ptr::null_mut(), Ordering::SeqCst);
        // SAFETY: from CString::into_raw in new(); no handler can read it now.
        unsafe {
            drop(std::ffi::CString::from_raw(self.page));
        }
    }
}

/// `eve-bridge web open` (spec M4, the Mac's sign-in): this Mac's browser, signed in to eve. A one-time code comes
/// over the 0600 IPC socket and goes into a page in this user's own temporary folder (0600) that sends the browser
/// on to `http://localhost:<port>/#login=<code>`; `open` opens that page. So the code is never printed, logged or in
/// a command line. The page is deleted once the browser signed in with it, once the code ran out, and on Ctrl-C,
/// SIGTERM or SIGHUP. One line on stdout names the page's address (install-lite.sh reads its port).
pub fn run_web_open(wait: f64, ctx: &Ctx, io: &Io) -> i32 {
    let sock = ctx.lite_socket();
    let deadline = Instant::now() + Duration::from_secs_f64(wait);
    let mut asked_launchd = false;
    let answer = loop {
        let answer = lite_ask(&sock, &json!({"evt": "web_login"}), 5.0, io);
        if let Some(a) = &answer {
            if py::is_true(get(a, "ok")) {
                break a.clone();
            }
        }
        if answer.is_none() && !asked_launchd {
            asked_launchd = true;
            if !ctx.lite_loaded() {
                io.warn(LITE_NOT_LOADED);
                return 1;
            }
        }
        if Instant::now() >= deadline {
            lite_answered(answer.as_ref(), io);
            return 1;
        }
        std::thread::sleep(Duration::from_secs_f64(ctx.poll_secs));
    };
    let issued = Instant::now();
    let (code, port) = (get(&answer, "code"), get(&answer, "port"));
    let code = match code {
        Value::String(c) if eve_web_auth::TOKEN.is_match(c) && is_port(port) => c.clone(),
        _ => {
            io.warn(LITE_NO_CODE);
            return 1;
        }
    };
    let url = format!("http://localhost:{}", pstr(port));
    let made = tempfile::Builder::new().prefix("eve-open-").suffix(".html").tempfile_in(
        user_tmp(&ctx.env).map(PathBuf::from).unwrap_or_else(std::env::temp_dir),
    );
    let (mut file, page) = match made.and_then(|f| f.keep().map_err(|e| e.error)) {
        Ok(kept) => kept,
        Err(e) => {
            io.warn(&format!(
                "eve-bridge: the sign-in page could not be written ({}).",
                eve_ipc::io_error_type_name(&e)
            ));
            return 1;
        }
    };
    let rc = (|| {
        if let Err(e) = file.write_all(&login_page(&format!("{url}/#login={code}"))).and_then(|()| file.flush()) {
            io.warn(&format!("eve-bridge: {}: {e}", eve_ipc::io_error_type_name(&e)));
            return 1;
        }
        drop(file);
        let _signals = ctx.catch_signals.then(|| StopOnSignals::new(&page));
        let page_text = page.to_string_lossy().into_owned();
        let opened = match &ctx.open_in_browser {
            Some(open) => open(&page_text),
            None => open_in_browser(&page_text, ctx),
        };
        if !opened {
            io.warn(LITE_NO_BROWSER);
            return 1;
        }
        io.say(&format!("eve is open in your browser: {url}"));
        if !login_used(&sock, &code, issued + Duration::from_secs_f64(ctx.login_secs), ctx, io) {
            io.warn(LITE_LOGIN_UNUSED);
        }
        0
    })();
    let _ = std::fs::remove_file(&page);
    rc
}

/// `_login_used`: wait until the browser signed in with `code` (lite.py's web_login_used), or until `until`.
pub fn login_used(sock: &str, code: &str, until: Instant, ctx: &Ctx, io: &Io) -> bool {
    loop {
        let answer = lite_ask(sock, &json!({"evt": "web_login_used", "code": code}), 5.0, io);
        if answer.as_ref().is_some_and(|a| py::is_true(get(a, "used"))) {
            return true;
        }
        if Instant::now() >= until {
            return false;
        }
        std::thread::sleep(Duration::from_secs_f64(ctx.poll_secs));
    }
}

/// `_login_page`: a page that sends the browser on to `url` at once and leaves no history entry
/// (location.replace), with a meta refresh where scripts are off. `url` was checked (a port, and a code of URL-safe
/// characters), so it needs no escaping in either place.
pub fn login_page(url: &str) -> Vec<u8> {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"referrer\" content=\"no-referrer\">\
         <meta http-equiv=\"refresh\" content=\"0; url={url}\">\
         <title>Opening eve</title></head><body>\
         <script>location.replace({})</script>\
         <p>Opening eve…</p></body></html>",
        py::dumps(&json!(url), None, true)
    )
    .into_bytes()
}

/// `_user_tmp`: this user's own temporary folder: $TMPDIR, else the one macOS keeps for the user (a start from
/// launchd or AppleScript may have no TMPDIR, or one that is gone, and /tmp is every account's); None, the default
/// choice, where neither is a folder.
pub fn user_tmp(env: &HashMap<String, String>) -> Option<String> {
    let folder = env.get("TMPDIR").cloned().unwrap_or_default();
    if !folder.is_empty() && Path::new(&folder).is_dir() {
        return Some(folder);
    }
    #[cfg(target_os = "macos")]
    {
        let mut buf = vec![0 as libc::c_char; 1024];
        // SAFETY: confstr writes at most buf.len() bytes, NUL-terminated.
        let n = unsafe { libc::confstr(CS_DARWIN_USER_TEMP_DIR, buf.as_mut_ptr(), buf.len()) };
        if n > 0 && n <= buf.len() {
            // SAFETY: confstr NUL-terminated what it wrote.
            let mine = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) }.to_string_lossy().into_owned();
            if !mine.is_empty() && Path::new(&mine).is_dir() {
                return Some(mine);
            }
        }
    }
    None
}

/// `_default_browser`: the bundle id of the app LaunchServices opens http links with; None when its record names
/// none (Safari, the default) or cannot be read.
pub fn default_browser(home: &Path) -> Option<String> {
    let data = plist::Value::from_file(home.join(LAUNCH_SERVICES)).ok()?;
    let handlers = data.as_dictionary()?.get("LSHandlers")?.as_array()?;
    for scheme in ["http", "https"] {
        for handler in handlers {
            let Some(h) = handler.as_dictionary() else { continue };
            let named = match h.get("LSHandlerURLScheme") {
                None => String::new(),
                Some(plist::Value::String(s)) => s.clone(),
                Some(plist::Value::Integer(i)) => i.to_string(),
                Some(plist::Value::Boolean(b)) => if *b { "True" } else { "False" }.to_string(),
                Some(_) => String::new(),
            };
            if named.to_lowercase() != scheme {
                continue;
            }
            if let Some(plist::Value::String(bundle)) = h.get("LSHandlerRoleAll") {
                if bundle.contains('.') && BUNDLE_ID.is_match(bundle) {
                    return Some(bundle.clone());
                }
            }
        }
    }
    None
}

/// `_open_in_browser`: open the page at `path` in the default browser, by the browser's bundle id (the app that
/// opens .html files may be an editor). Plain `open` when no browser is named, or that one does not start.
pub fn open_in_browser(path: &str, ctx: &Ctx) -> bool {
    let mut tries: Vec<Vec<String>> = Vec::new();
    if let Some(browser) = default_browser(&ctx.home) {
        tries.push(vec![OPEN.into(), "-b".into(), browser, path.into()]);
    }
    tries.push(vec![OPEN.into(), path.into()]);
    tries.iter().any(|argv| matches!((ctx.run)(argv, Duration::from_secs(30)), Ok((0, _))))
}

// ---- web devices, revoke, lan --------------------------------------------------------------------------------

/// `_when`: a stamp as `YYYY-MM-DD HH:MM`, local; "?" for anything else.
pub fn when(stamp: &Value) -> String {
    let secs = match stamp {
        Value::Number(n) => n.as_f64(),
        _ => None,
    };
    match secs {
        Some(s) if s > 0.0 => py::local_strftime(s, "%Y-%m-%d %H:%M").unwrap_or_else(|| "?".into()),
        _ => "?".into(),
    }
}

/// `format(s, "<width")`.
fn pad(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - n))
    }
}

/// `eve-bridge web devices`: the browsers that can open eve, oldest first, by id, kind and browser.
pub fn run_web_devices(ctx: &Ctx, io: &Io) -> i32 {
    let answer = lite_ask(&ctx.lite_socket(), &json!({"evt": "devices"}), 5.0, io);
    if !lite_answered(answer.as_ref(), io) {
        return 1;
    }
    let answer = answer.unwrap_or_default();
    let devices: Vec<&Value> = match get(&answer, "devices") {
        Value::Array(a) => a.iter().filter(|d| d.is_object()).collect(),
        _ => Vec::new(),
    };
    if devices.is_empty() {
        io.say(
            "No browser can open eve yet. On this Mac, `eve-bridge web open`; for a phone, on eve's page: Settings, \
             Phone access, Add a phone.",
        );
        return 0;
    }
    io.say(&format!("{} browser{} can open eve:", devices.len(), if devices.len() != 1 { "s" } else { "" }));
    for d in devices {
        io.say(&format!(
            "  {}  {}  {}  paired {}, last seen {}",
            pad(&shown(get(d, "id"), 8), 8),
            pad(&shown(get(d, "label"), 64), 24),
            shown(get(d, "host"), 64),
            when(get(d, "created")),
            when(get(d, "last_seen"))
        ));
    }
    io.say("`eve-bridge web revoke <id>` signs one out; `eve-bridge web revoke all` signs them all out.");
    0
}

/// `eve-bridge web revoke <id|all>`: sign browsers out; their open links close (lite.py's revoke).
pub fn run_web_revoke(target: &str, ctx: &Ctx, io: &Io) -> i32 {
    let wanted = eve_envfile::py_strip(target).to_lowercase();
    if !(wanted == "all" || eve_web_auth::DEVICE_ID.is_match(&wanted)) {
        io.warn("eve-bridge: give a browser's id from `eve-bridge web devices`, or all.");
        return 2;
    }
    let answer = lite_ask(&ctx.lite_socket(), &json!({"evt": "revoke", "id": wanted}), 30.0, io);
    if !lite_answered(answer.as_ref(), io) {
        return 1;
    }
    let answer = answer.unwrap_or_default();
    let gone: Vec<String> = match get(&answer, "revoked") {
        Value::Array(a) => a.iter().filter(|i| i.is_string()).map(|i| shown(i, 8)).collect(),
        _ => Vec::new(),
    };
    if !gone.is_empty() {
        io.say(&format!("Signed out of eve: {}", gone.join(", ")));
        return 0;
    }
    if wanted == "all" {
        io.say("No browser was signed in; nothing changed.");
        return 0;
    }
    io.say(&format!("No browser has the id {wanted}; nothing changed. `eve-bridge web devices` lists them."));
    1
}

/// `eve-bridge web lan on|off`: eve's door for phones and laptops on the Wi-Fi, remembered across restarts.
pub fn run_web_lan(on: bool, ctx: &Ctx, io: &Io) -> i32 {
    let answer = lite_ask(&ctx.lite_socket(), &json!({"evt": "lan", "on": on}), 30.0, io);
    if !lite_answered(answer.as_ref(), io) {
        return 1;
    }
    let lan = answer.as_ref().is_some_and(|a| py::is_true(get(a, "lan")));
    if lan {
        io.say(
            "Phone access is on: a phone or laptop on this Wi-Fi can open eve once it is paired (on eve's page: \
             Settings, Phone access, Add a phone).",
        );
    } else {
        io.say("Phone access is off: only this Mac's browser can open eve.");
    }
    if lan == on {
        0
    } else {
        1
    }
}

// ---- status and doctor ---------------------------------------------------------------------------------------

fn is_file(path: &Path) -> bool {
    path.is_file()
}

/// `eve-bridge status` in eve lite: yes or no for each part, and nothing besides (no key, path, port or address).
/// The daemon's own answer when it runs; the sign-ins are read from this Mac's files when it does not.
pub fn lite_status(ctx: &Ctx, io: &Io) -> i32 {
    let answer = lite_ask(&ctx.lite_socket(), &json!({"evt": "status"}), 5.0, io);
    let live = answer.as_ref().is_some_and(|a| py::is_true(get(a, "ok")));
    let st = if live { answer.unwrap_or_default() } else { json!({}) };
    let flag = |name: &str| py::is_true(get(&st, name));
    let rows: Vec<(&str, bool)> = vec![
        ("login service installed", ctx.launchd.lite_installed().unwrap_or(false)),
        ("login service loaded", ctx.lite_loaded()),
        ("eve answering", live),
        ("OpenRouter signed in", if live { flag("openrouter") } else { eve_openrouter::available(Some(&ctx.env)) }),
        (
            "Composio signed in",
            if live { flag("composio") } else { is_file(&ctx.expand(eve_brain::onboarding::CONNECT_TOKEN_FILE)) },
        ),
        ("chat on", flag("chat")),
        ("voice ready", flag("voice")),
        ("phone access on", flag("lan")),
    ];
    let width = rows.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    io.say("eve lite on this Mac:");
    for (name, value) in rows {
        io.say(&format!("  {}  {}", pad(name, width), if value { "yes" } else { "no" }));
    }
    0
}

/// `_env_file_profile`: the EVE_PROFILE the env file at `path` sets; "" when it sets none or is not there,
/// "unreadable" when it cannot be read.
pub fn env_file_profile(path: &Path) -> String {
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return String::new(),
        Err(_) => return "unreadable".into(),
    };
    let Ok(text) = String::from_utf8(bytes) else { return "unreadable".into() };
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    eve_envfile::parse_env_file(text).get(eve_profile::PROFILE_ENV).cloned().unwrap_or_default()
}

/// `_sets_lite`: whether this EVE_PROFILE value is lite (an invalid one is not).
pub fn sets_lite(value: &str) -> bool {
    let env: HashMap<String, String> = [(eve_profile::PROFILE_ENV.to_string(), value.to_string())].into();
    eve_profile::is_lite(Some(&env)).unwrap_or(false)
}

/// `_loose`: whether `path`, when it is there, lets another account in (a group or other bit set).
pub fn loose(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).map(|m| m.permissions().mode() & 0o077 != 0).unwrap_or(false)
}

fn executable(path: &str) -> bool {
    use std::os::unix::ffi::OsStrExt;
    if !Path::new(path).is_file() {
        return false;
    }
    let Ok(c) = std::ffi::CString::new(Path::new(path).as_os_str().as_bytes()) else { return false };
    // SAFETY: access() reads a NUL-terminated path.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// `eve-bridge doctor`: eve lite on this Mac, part by part, each with what to do about it, marked ok, -- (nothing
/// to do, or optional) or !! (to fix); exit 1 while anything is to fix. It prints no key, code or token.
pub fn lite_doctor(ctx: &Ctx, io: &Io) -> i32 {
    let mut rows: Vec<(&str, String)> = Vec::new();
    let mut say = |mark: &'static str, line: String| rows.push((mark, line));

    // the profile: the env file is what makes `eve-bridge` in a Terminal eve lite's
    let env_file = ctx.env_file();
    if sets_lite(&env_file_profile(&env_file)) {
        say(OK, format!("profile: {} sets EVE_PROFILE=lite", ctx.tilde(&env_file)));
    } else {
        say(
            FIX,
            format!(
                "profile: {} does not set EVE_PROFILE=lite, so `eve-bridge` in a Terminal runs full eve's commands. \
                 Run eve lite's install line again.",
                ctx.tilde(&env_file)
            ),
        );
    }

    // the login service, and full eve beside it
    let plist = ctx.launchd.installed_lite_plist();
    let loaded = ctx.lite_loaded();
    match plist {
        None => say(FIX, "login service: not installed. `eve-bridge install --service` installs it.".into()),
        Some(plist) => {
            let pinned = match plist.get("EnvironmentVariables").and_then(plist::Value::as_dictionary) {
                Some(env) => {
                    let v = match env.get(eve_profile::PROFILE_ENV) {
                        Some(plist::Value::String(s)) => s.clone(),
                        _ => String::new(),
                    };
                    sets_lite(&v)
                }
                None => false,
            };
            let program = plist
                .get("ProgramArguments")
                .and_then(plist::Value::as_array)
                .and_then(|a| a.first())
                .and_then(plist::Value::as_string)
                .map(str::to_string);
            if !pinned {
                say(
                    FIX,
                    "login service: it does not pin EVE_PROFILE=lite. `eve-bridge install --service` writes it again."
                        .into(),
                );
            } else if !program.as_deref().is_some_and(executable) {
                say(FIX, "login service: the Python it starts is gone. Run eve lite's install line again.".into());
            } else if !loaded {
                say(FIX, "login service: installed, not loaded. `eve-bridge install --service` loads it.".into());
            } else {
                say(OK, "login service: installed and loaded".into());
            }
        }
    }
    if ctx.home.join("Library/LaunchAgents").join(format!("{}.plist", eve_lite::FULL_LABEL)).exists() {
        say(
            FIX,
            format!(
                "full eve's login service is installed for this user ({}), so eve lite stays down: a Mac account has \
                 one of them.",
                eve_lite::FULL_LABEL
            ),
        );
    }

    // the daemon, by its own status
    let answer = lite_ask(&ctx.lite_socket(), &json!({"evt": "status"}), 5.0, io);
    let live = answer.as_ref().is_some_and(|a| py::is_true(get(a, "ok")));
    let st = if live { answer.unwrap_or_default() } else { json!({}) };
    let yes = |name: &str| py::is_true(get(&st, name));
    if !live {
        say(
            FIX,
            format!(
                "eve: not answering on this Mac. Its log is ~/Library/Logs/eve-lite.log{}.",
                if loaded { "" } else { "; `eve-bridge install --service` starts it" }
            ),
        );
    } else if yes("restarting") {
        say(INFO, "eve: restarting into a new sign-in; run this again in a few seconds".into());
    } else if get(&st, "state") == &json!(eve_lite::READY) {
        if yes("chat") {
            say(OK, "eve: running and set up".into());
        } else {
            say(FIX, "eve: running, but its chat did not start. Its log is ~/Library/Logs/eve-lite.log.".into());
        }
    } else {
        say(INFO, "eve: running, waiting for the OpenRouter sign-in".into());
    }
    let openrouter = if live { yes("openrouter") } else { eve_openrouter::available(Some(&ctx.env)) };
    if !openrouter {
        say(FIX, "OpenRouter: not signed in. `eve-bridge web open`, then Sign in with OpenRouter.".into());
    } else if live && yes("openrouter_rejected") {
        say(
            FIX,
            "OpenRouter: the key was turned down (it was deleted or revoked). `eve-bridge web open`, then Sign in \
             with OpenRouter again."
                .into(),
        );
    } else {
        say(OK, "OpenRouter: signed in".into());
    }
    let composio =
        if live { yes("composio") } else { is_file(&ctx.expand(eve_brain::onboarding::CONNECT_TOKEN_FILE)) };
    if composio {
        say(OK, "Composio: signed in".into());
    } else {
        say(INFO, "Composio: not signed in (optional: on eve's page, Settings, Apps)".into());
    }
    if live {
        let (port, lan_port) = (get(&st, "port"), get(&st, "lan_port"));
        if yes("page") && is_port(port) {
            say(OK, format!("the Mac's page: http://localhost:{}", pstr(port)));
        } else {
            say(FIX, "the Mac's page: not up. If this stays so, every port it may use is taken: restart the Mac.".into());
        }
        if yes("lan") {
            let p = if is_port(lan_port) { format!(", port {}", pstr(lan_port)) } else { String::new() };
            say(OK, format!("phone access: on{p}"));
        } else {
            say(INFO, "phone access: off (on eve's page: Settings, Phone access, Add a phone)".into());
        }
        let network = get(&st, "local_network");
        if network == &json!("ok") {
            say(OK, "Local Network: allowed".into());
        } else if network == &json!("blocked") {
            say(
                FIX,
                "Local Network: blocked, so voice cannot reach a phone (chat still works over HTTPS). System \
                 Settings, Privacy & Security, Local Network: turn on python3.13."
                    .into(),
            );
        } else {
            say(INFO, "Local Network: not known yet (eve checks it when a phone pairs)".into());
        }
        if yes("voice") {
            say(OK, "voice: ready".into());
        } else {
            say(INFO, "voice: off (it needs the OpenRouter sign-in; see ~/Library/Logs/eve-lite.log)".into());
        }
        let mut refused: Vec<(&String, &Value)> = match get(&st, "firewall") {
            Value::Object(m) => m.iter().collect(),
            _ => Vec::new(),
        };
        refused.sort_by(|a, b| a.0.cmp(b.0));
        let named: Vec<String> = refused
            .into_iter()
            .filter_map(|(k, v)| match v {
                Value::Number(n) if n.is_i64() || n.is_u64() => {
                    let tail = k.split_once(':').map(|(_, t)| t).unwrap_or(k);
                    Some(format!("{} ({})", shown_str(tail, 64), n))
                }
                _ => None,
            })
            .collect();
        if named.is_empty() {
            say(OK, "firewall: nothing refused".into());
        } else {
            say(
                INFO,
                format!(
                    "firewall: refused {}. eve lite starts no other program and stays out of private folders; \
                     nothing to do unless something you use stopped working.",
                    named.join(", ")
                ),
            );
        }
    }

    // the files: the page, and the private ones
    let named_root = eve_envfile::py_strip(ctx.var(eve_lite_web::WEB_DIR_ENV)).to_string();
    let web_root = ctx.expand(if named_root.is_empty() { eve_lite_web::PACKAGED_WEB_DIR } else { &named_root });
    if web_root.join("index.html").is_file() {
        say(OK, "web page: installed".into());
    } else {
        say(FIX, "web page: not installed. Run eve lite's install line again.".into());
    }
    let tls = ctx.expand(eve_lite::TLS_DIR);
    let private = [
        env_file.clone(),
        ctx.expand(eve_brain::onboarding::CONNECT_TOKEN_FILE),
        ctx.expand(eve_lite::DEVICES_FILE),
        ctx.expand(eve_lite_web::WEB_FILE),
        tls.clone(),
        tls.join("ca.key"),
        tls.join("leaf.key"),
    ];
    let open_to_others: Vec<String> = private.iter().filter(|p| loose(p)).map(|p| ctx.tilde(p)).collect();
    if open_to_others.is_empty() {
        say(OK, "private files: this account's alone".into());
    } else {
        say(
            FIX,
            format!(
                "private files: other accounts on this Mac can read {}. `chmod 600` each file (700 a folder).",
                open_to_others.join(", ")
            ),
        );
    }
    let exe = std::env::current_exe().unwrap_or_default();
    say(INFO, format!("eve-bridge {}: {}", eve_meta::VERSION, ctx.tilde(&exe)));
    say(
        INFO,
        format!(
            "Xcode's command line tools: {} (eve lite does not use them)",
            if eve_toolchain::dev_tools_present(Some(&ctx.env)) { "installed" } else { "not installed" }
        ),
    );

    io.say("eve lite on this Mac:");
    for (mark, line) in &rows {
        io.say(&format!("  {mark}  {line}"));
    }
    let fixes = rows.iter().filter(|(m, _)| *m == FIX).count();
    io.say(&if fixes == 0 { "Nothing to fix.".to_string() } else { format!("{fixes} to fix, marked {FIX}.") });
    if fixes > 0 {
        1
    } else {
        0
    }
}

// ---- lite uninstall ------------------------------------------------------------------------------------------

fn lexists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(bytes))
}

/// `_kept_by_lite`: the folder is as eve lite's uninstall kept it, that is the marker names the env file's SHA-256
/// (or none, with no env file) and the env file has not changed since.
pub fn kept_by_lite(kept: &Path, env_file: &Path) -> bool {
    if !kept.is_file() {
        return false;
    }
    let Ok(text) = std::fs::read_to_string(kept) else { return false };
    let mut want = String::new();
    for line in eve_envfile::py_splitlines(&text) {
        if let Some(v) = line.strip_prefix("env_sha256=") {
            want = v.to_string();
        }
    }
    let got = if lexists(env_file) {
        match std::fs::read(env_file) {
            Ok(b) => sha256_hex(&b),
            Err(_) => return false,
        }
    } else {
        "none".to_string()
    };
    !want.is_empty() && want == got
}

/// `_mark_kept`: the marker that lets the install line take the kept folder back, while the env file is what this
/// left (its SHA-256), or there is none. Written whole (0600), as the installer writes it.
pub fn mark_kept(kept: &Path, env_file: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let digest = if lexists(env_file) { sha256_hex(&std::fs::read(env_file)?) } else { "none".to_string() };
    let text = format!(
        "# eve lite was uninstalled and kept this folder. Its install line takes the folder back while env is\n\
         # what the uninstall left: the file with this SHA-256, or none at all.\n\
         env_sha256={digest}\n"
    );
    let dir = kept.parent().unwrap_or(Path::new("."));
    let mut tmp = tempfile::Builder::new().prefix(&format!("{LITE_KEPT}.")).suffix("").tempfile_in(dir)?;
    tmp.write_all(text.as_bytes())?;
    std::fs::set_permissions(tmp.path(), std::fs::Permissions::from_mode(0o600))?;
    tmp.persist(kept).map_err(|e| e.error)?;
    Ok(())
}

/// `_app_is_ours`: `app` is the Eve.app install-lite.sh made, whose script runs an eve lite venv's eve-bridge (read
/// with osadecompile, as the installer reads it). Any other Eve.app is someone else's, and stays.
pub fn app_is_ours(app: &Path, ctx: &Ctx) -> bool {
    if !app.is_dir() {
        return false;
    }
    match (ctx.run)(&[OSADECOMPILE.into(), app.to_string_lossy().into_owned()], Duration::from_secs(20)) {
        Ok((0, out)) => out.contains("/.eve-lite/venv/bin/eve-bridge"),
        _ => false,
    }
}

/// `_has_content`: whether the file holds any line that is not blank (a comment counts); an unreadable one does,
/// so it is kept.
pub fn has_content(path: &Path) -> bool {
    match std::fs::read(path) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(_) => true,
        Ok(b) => match String::from_utf8(b) {
            Err(_) => true,
            Ok(text) => {
                let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
                eve_envfile::py_splitlines(text).iter().any(|l| !eve_envfile::py_strip(l).is_empty())
            }
        },
    }
}

/// `_remove`: remove `path`, a file, a link (never what it points to) or a folder; true once it is gone. A failure
/// is said on stderr.
pub fn remove(path: &Path, ctx: &Ctx, io: &Io) -> bool {
    let is_link = std::fs::symlink_metadata(path).map(|m| m.file_type().is_symlink()).unwrap_or(false);
    let result = if is_link || !path.is_dir() { std::fs::remove_file(path) } else { std::fs::remove_dir_all(path) };
    match result {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => true,
        Err(e) => {
            io.warn(&format!(
                "eve-bridge: {} was not removed ({}); delete it yourself.",
                ctx.tilde(path),
                eve_ipc::io_error_type_name(&e)
            ));
            false
        }
    }
}

/// `_yes`: y or yes typed at the terminal; no terminal means no (as install-lite.sh asks).
pub fn yes(question: &str, io: &Io) -> bool {
    if !io.stdin_tty {
        return false;
    }
    let answer = io.ask(question).unwrap_or_default();
    ["y", "yes"].contains(&eve_envfile::py_strip(&answer).to_lowercase().as_str())
}

/// The exception's type name for a line on stderr (an OSError's own, or ValueError).
fn error_type(e: &anyhow::Error) -> String {
    if let Some(io) = e.downcast_ref::<std::io::Error>() {
        return eve_ipc::io_error_type_name(io).to_string();
    }
    if let Some(env) = e.downcast_ref::<eve_envfile::EnvFileError>() {
        return match env {
            eve_envfile::EnvFileError::Io(io) => eve_ipc::io_error_type_name(io).to_string(),
            eve_envfile::EnvFileError::Decode(_) => "UnicodeDecodeError".into(),
            eve_envfile::EnvFileError::HomeDir => "RuntimeError".into(),
            _ => "ValueError".into(),
        };
    }
    "OSError".into()
}

/// `eve-bridge lite uninstall`: what `install-lite.sh --uninstall` does, from eve lite itself. It stops eve lite and
/// removes its login service, Eve.app (only the one the installer made), ~/.eve-lite and eve lite's log. The env
/// file loses its EVE_PROFILE=lite line. ~/.config/eve-bridge holds eve's memory: when it is eve lite's alone it asks
/// before deleting it, and keeps it when there is no terminal to ask; kept, it gets the installer's marker.
/// ~/.eve-lite goes last. A part that cannot be removed is said, the rest still goes, and the exit is 1.
pub fn lite_uninstall(dry_run: bool, ctx: &Ctx, io: &Io) -> i32 {
    let home = &ctx.home;
    let (lite_home, app) = (home.join(".eve-lite"), home.join("Applications").join("Eve.app"));
    let env_dir = home.join(".config").join("eve-bridge");
    let (env_file, kept) = (env_dir.join("env"), env_dir.join(LITE_KEPT));
    let plist = ctx.launchd.lite_plist_path().unwrap_or_else(|_| home.join(format!("Library/LaunchAgents/{LITE_LABEL}.plist")));
    let log = ctx.launchd.lite_log_path().unwrap_or_else(|_| home.join("Library/Logs/eve-lite.log"));
    let sets = sets_lite(&env_file_profile(&env_file));
    let full = ctx.launchd.agent_path(LABEL).map(|p| p.exists()).unwrap_or(false);
    let ours = !full && (sets || kept_by_lite(&kept, &env_file));
    let app_ours = app_is_ours(&app, ctx);
    let mut removed: Vec<PathBuf> = [&plist, &lite_home, &log].into_iter().filter(|p| lexists(p)).cloned().collect();
    if app_ours {
        removed.push(app.clone());
    }

    if dry_run {
        let head = "eve-bridge lite uninstall, dry run. It would stop eve lite's login service if it is loaded";
        io.say(&format!(
            "{head}{}",
            if removed.is_empty() { "; nothing else of eve lite's is left to remove." } else { ", and remove:" }
        ));
        for path in &removed {
            io.say(&format!("  {}", ctx.tilde(path)));
        }
        if ours {
            io.say(&format!(
                "Then it would ask before deleting ~/.config/eve-bridge, which holds eve's memory. If it is kept, {}and \
                 the install line takes it back.",
                if sets {
                    "the folder loses only the EVE_PROFILE=lite line (the env file goes when nothing else is in it), "
                } else {
                    "the folder stays as an earlier uninstall left it, "
                }
            ));
        } else if sets {
            io.say("It would also take the EVE_PROFILE=lite line out of ~/.config/eve-bridge/env, which full eve keeps.");
        }
        io.say("Nothing was written.");
        return 0;
    }

    let forget_all = ours && yes("Also delete eve's memory, chats and sign-ins in ~/.config/eve-bridge? [y/N] ", io);
    let mut rc = 0;
    // the service first, so nothing runs from ~/.eve-lite while it goes
    if let Err(e) = ctx.launchd.uninstall_lite() {
        io.warn(&format!("eve-bridge: eve lite's login service was not removed ({}).", error_type(&e)));
        rc = 1;
    }
    let mut parts: Vec<&Path> = Vec::new();
    if app_ours {
        parts.push(&app);
    }
    parts.push(&log);
    for path in parts {
        if !remove(path, ctx, io) {
            rc = 1;
        }
    }
    let final_line;
    if forget_all {
        if !remove(&env_dir, ctx, io) {
            rc = 1;
        }
        final_line = "eve lite is uninstalled, with its memory and settings.";
    } else {
        let changed = (|| -> anyhow::Result<()> {
            if sets {
                eve_envfile::remove_key(&env_file, eve_profile::PROFILE_ENV)?;
                if ours && !has_content(&env_file) {
                    // it held nothing but the profile
                    match std::fs::remove_file(&env_file) {
                        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                        _ => {}
                    }
                }
            }
            if full {
                // the folder is full eve's: nothing of eve lite's takes it back
                match std::fs::remove_file(&kept) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            } else if ours {
                mark_kept(&kept, &env_file)?;
            }
            Ok(())
        })();
        if let Err(e) = changed {
            io.warn(&format!(
                "eve-bridge: ~/.config/eve-bridge/env could not be changed ({}): take its EVE_PROFILE line out \
                 yourself.",
                error_type(&e)
            ));
            rc = 1;
        }
        final_line = if full {
            "eve lite is uninstalled. Full eve stays as it is."
        } else if ours {
            "eve lite is uninstalled. Its memory and settings stay in ~/.config/eve-bridge; the install line takes \
             them back."
        } else {
            "eve lite is uninstalled."
        };
    }
    if !remove(&lite_home, ctx, io) {
        rc = 1;
    }
    io.say(final_line);
    rc
}
