//! The owner's Fire TV: power, volume, the d-pad, play/pause, apps by name, typing, and a look at the screen —
//! over the LAN, with network ADB.
//!
//! Port of `eve_bridge/firetv.py`.
//!
//! The owner asked on 2026-10-04 to control their Fire TV smart TV from eve "the most developer-friendly way". That
//! is ADB over the network: the TV's own debugging bridge, which Amazon documents for Fire TV and which needs no
//! cloud, no hub and no app on the TV. Eve shells out to Google's `adb` (Homebrew's `android-platform-tools`),
//! which keeps the key this Mac was allowed with in `~/.android/adbkey`.
//!
//! What each thing costs on the TV was measured on 2026-10-04, and the commands are chosen by it:
//!
//! | What | Command on the TV | Time | Read back |
//! |---|---|---|---|
//! | state | `dumpsys power` + `dumpsys window` + the `dumpsys audio` STREAM_MUSIC block, one round trip | ~0.5 s | is the read |
//! | play / pause / ... | `cmd media_session dispatch play-pause` etc. | ~75 ms | no |
//! | volume | `cmd media_session volume --stream 3 --set N` | ~75 ms | yes, from dumpsys |
//! | mute / unmute | `input keyevent 164` (a toggle: sent only when the state differs) | ~1.2 s | yes, from dumpsys |
//! | d-pad, back, home | `input keyevent 20 20 23` — several keys, one call | ~1.2 s | no |
//! | power | `input keyevent 224` (WAKEUP) / `223` (SLEEP) | ~1.2 s | yes, polled |
//! | open an app | `am start -n package/activity` | ~120 ms | no |
//! | YouTube search | `am start -a VIEW -n <YouTube> -d <results URL>` | ~120 ms | no |
//! | type text | `input text` | ~1.2 s | no |
//! | screenshot | `adb exec-out screencap -p` | ~2.3 s | is the picture |
//!
//! `input` costs about 1.2 s because it starts a Java VM on the TV each time, so it is used only where nothing
//! faster exists, and a run of key presses ("down three times") goes in one call. Mute is `input` and not
//! `cmd media_session dispatch mute`: that one sends the *microphone* mute key (KEYCODE_MUTE, AOSP
//! MediaShellCommand), which never reaches the speaker. Power is WAKEUP and SLEEP, which do nothing when the TV is
//! already there; the POWER key is a toggle and is never sent. The volume is never written by a raw binder
//! transaction: one set this TV's volume to 0 on 2026-10-04.
//!
//! The connection. `~/.config/eve-bridge/firetv.json` (mode 600) keeps the TV's address and its mDNS name
//! (`adb-<serial>`), written by `eve-bridge firetv pair`. Before the first command, and after a failure, eve runs
//! `adb connect` (whose exit code is always 0, so its words are read) and then `get-state`, which must say
//! `device`. When the TV has gone (offline, not found, a timeout) eve disconnects, finds it again by its mDNS name
//! (a new DHCP address is saved), connects and retries once, then gives up with a plain line. The retry is safe
//! when adb said the TV was gone before the command ran; after a timeout the command may already have landed, so
//! only reads and commands that land the same twice (WAKEUP, SLEEP, a volume level, opening an app) are sent
//! again: a second `input text` would type the words twice. It restarts the adb server only in one case: adb says
//! the TV cannot be reached while a plain TCP connection to the TV's port opens at once. Then the TV is fine and
//! the adb server is stale (it lost its route after the Mac's network changed, 2026-10-08: "No route to host" for
//! hours with the TV on), so `kill-server` and `start-server` run once and eve connects again. At most once every
//! [`ADB_RESTART_EVERY`] seconds, and never otherwise: something else on the Mac may be using the server. Three
//! outcomes are kept apart, the way the Muse gadget taught: the TV answered, the TV allowed this Mac, the command
//! was done.
//!
//! Standby. The TV keeps answering for about 6 minutes after it is turned off; then its chip suspends, and a
//! network packet waking it from there is unproven. So "turn on the TV" works from the screensaver and from a
//! fresh standby, and from deep standby says so and suggests the remote or Alexa. HDMI inputs cannot be switched:
//! the TV's input switcher needs an Amazon-signed permission the shell does not have.
//!
//! Four doors use one [`FireTV`]: Telegram (a message that is only a TV command and names the TV is done by code,
//! [`match_command`]; anything else goes to the model with `firetv_status`, `firetv_control` and
//! `firetv_screenshot`), the robot's live voice (status and control, no screenshot), the terminal
//! (`eve-bridge firetv ...`, [`cli`]) and the Mini App's remote card ([`FireTV::panel`] behind `/api/firetv`).
//!
//! Everything the TV reports (app names, the model, errors) is cleaned of control characters and capped before it
//! reaches a model or a log; the log keeps the action, the outcome and the time, never typed text or a search.
//!
//! `EVE_FIRETV=0` turns it all off. With no pairing it is off too, and no tool is lent.

use std::collections::{BTreeSet, HashMap};
use std::future::Future;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};
use tokio::io::AsyncReadExt;

pub mod pyfmt;

use pyfmt::{py_int, py_repr_str, py_round, py_str, py_truthy};

/// A JSON object, the shape every Python answer dict has. Key order is kept (`preserve_order`).
pub type Out = Map<String, Value>;
/// The paired TV's config: `host`, `port`, `mdns`, `model`, and whatever else the file holds.
pub type Config = Map<String, Value>;
/// The environment to read switches from; `None` where a function takes it means the process environment.
pub type Environ = HashMap<String, String>;
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

pub const CONFIG_PATH: &str = "~/.config/eve-bridge/firetv.json";
pub const SHOTS_DIR: &str = "~/.cache/eve-bridge/firetv";
pub const OFF_REASON: &str = "The Fire TV is not set up on this computer (eve-bridge firetv pair; EVE_FIRETV)";
/// Said when a TV command arrives with no pairing: the code path still knows the words, so the model never gets
/// a TV request it has no tools for (the same lesson as Spotify's, 2026-09-27).
pub const NOT_SET_UP_LINE: &str = "The Fire TV isn't set up on this Mac yet. Turn on ADB debugging on the TV, run: \
                                   eve-bridge firetv pair, then restart eve.";
pub const INSTALL_LINE: &str = "adb isn't installed on this Mac. Run: brew install --cask android-platform-tools";
pub const ALLOW_LINE: &str = "The TV hasn't allowed this Mac yet. On the TV, accept the Allow USB debugging prompt \
                              and tick Always allow from this computer, then try again.";
pub const UNREACHABLE_LINE: &str = "I can't reach the TV. Is it on, and on the home network?";
pub const TIMEOUT_LINE: &str =
    "The TV didn't answer in time, so I don't know if that was done. I didn't send it again.";
pub const DEEP_STANDBY_LINE: &str = "I can't reach the TV. It is probably in deep standby (it stops answering \
                                     about 6 minutes after it is turned off). Use the remote, or ask Alexa to turn \
                                     it on.";
pub const DEFAULT_PORT: i64 = 5555;
/// The daemon runs under launchd, with a thin PATH.
pub const ADB_PATHS: [&str; 2] = ["/opt/homebrew/bin/adb", "/usr/local/bin/adb"];
pub const CONNECT_SECS: f64 = 6.0;
pub const SHELL_SECS: f64 = 8.0;
/// An `input` call starts a Java VM on the TV (~1.2 s), plus one per extra key.
pub const INPUT_SECS: f64 = 12.0;
pub const MDNS_SECS: f64 = 5.0;
/// The plain TCP check of the TV's port before an adb server restart.
pub const PROBE_SECS: f64 = 2.0;
/// At most one adb server restart per 10 minutes.
pub const ADB_RESTART_EVERY: f64 = 600.0;
pub const SCREENCAP_SECS: f64 = 20.0;
/// How long "turn on/off" watches the TV change after the key.
pub const POWER_POLL_SECS: f64 = 5.0;
pub const POLL_STEP_SECS: f64 = 0.5;
/// "turn it up" in percent; "a bit" is half.
pub const VOLUME_STEP: i64 = 10;
/// "down 40 times" is a typo or a loop, not a request.
pub const MAX_REPEAT: i64 = 10;
pub const MAX_TEXT: usize = 200;
pub const CLEAN_CAP: usize = 120;
/// The runner's code for "no answer in time" (as timeout(1)).
pub const RC_TIMEOUT: i32 = 124;
/// The runner's code for "adb would not start".
pub const RC_NO_ADB: i32 = 127;
pub const PNG_SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";

// ---- the seams -----------------------------------------------------------------------------------------------

/// (rc, combined stdout and stderr). A timeout is an answer ([`RC_TIMEOUT`]), never an error.
pub trait Runner: Send + Sync {
    fn run(&self, argv: Vec<String>, timeout: f64) -> BoxFuture<'_, (i32, String)>;
}

impl<F, Fut> Runner for F
where
    F: Fn(Vec<String>, f64) -> Fut + Send + Sync,
    Fut: Future<Output = (i32, String)> + Send + 'static,
{
    fn run(&self, argv: Vec<String>, timeout: f64) -> BoxFuture<'_, (i32, String)> {
        Box::pin(self(argv, timeout))
    }
}

/// (rc, stdout bytes): the screenshot, which is a PNG and must not be decoded.
pub trait Capture: Send + Sync {
    fn capture(&self, argv: Vec<String>, timeout: f64) -> BoxFuture<'_, (i32, Vec<u8>)>;
}

impl<F, Fut> Capture for F
where
    F: Fn(Vec<String>, f64) -> Fut + Send + Sync,
    Fut: Future<Output = (i32, Vec<u8>)> + Send + 'static,
{
    fn capture(&self, argv: Vec<String>, timeout: f64) -> BoxFuture<'_, (i32, Vec<u8>)> {
        Box::pin(self(argv, timeout))
    }
}

/// (host, port, timeout) -> whether a TCP connection opened: the TV answers, whatever adb says.
pub trait Probe: Send + Sync {
    fn probe(&self, host: String, port: i64, timeout: f64) -> BoxFuture<'_, bool>;
}

impl<F, Fut> Probe for F
where
    F: Fn(String, i64, f64) -> Fut + Send + Sync,
    Fut: Future<Output = bool> + Send + 'static,
{
    fn probe(&self, host: String, port: i64, timeout: f64) -> BoxFuture<'_, bool> {
        Box::pin(self(host, port, timeout))
    }
}

/// `asyncio.sleep`, replaceable in tests.
pub type Sleep = Arc<dyn Fn(f64) -> BoxFuture<'static, ()> + Send + Sync>;
/// `time.monotonic`, replaceable in tests.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;

fn secs(timeout: f64) -> Duration {
    Duration::try_from_secs_f64(timeout).unwrap_or(Duration::ZERO)
}

static MONO_START: LazyLock<Instant> = LazyLock::new(Instant::now);

/// Seconds on a monotonic clock (only differences mean anything, as with `time.monotonic`).
pub fn monotonic() -> f64 {
    MONO_START.elapsed().as_secs_f64()
}

fn default_sleep() -> Sleep {
    Arc::new(|s: f64| Box::pin(tokio::time::sleep(secs(s))) as BoxFuture<'static, ()>)
}

fn default_clock() -> Clock {
    Arc::new(monotonic)
}

pub async fn tcp_probe(host: &str, port: i64, timeout: f64) -> bool {
    let Ok(port) = u16::try_from(port) else {
        return false;
    };
    match tokio::time::timeout(secs(timeout), tokio::net::TcpStream::connect((host, port))).await {
        Ok(Ok(stream)) => {
            drop(stream);
            true
        }
        _ => false,
    }
}

/// The real probe: [`tcp_probe`].
#[derive(Debug, Clone, Copy, Default)]
pub struct TcpProbe;

impl Probe for TcpProbe {
    fn probe(&self, host: String, port: i64, timeout: f64) -> BoxFuture<'_, bool> {
        Box::pin(async move { tcp_probe(&host, port, timeout).await })
    }
}

// ---- running adb ---------------------------------------------------------------------------------------------

fn env_get(environ: Option<&Environ>, key: &str) -> String {
    match environ {
        Some(env) => env.get(key).cloned().unwrap_or_default(),
        None => std::env::var(key).unwrap_or_default(),
    }
}

fn executable(path: &str) -> bool {
    let Ok(c) = std::ffi::CString::new(path) else {
        return false;
    };
    // SAFETY: a valid NUL-terminated path; access(2) only reads it.
    unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 }
}

/// `EVE_ADB`, then the PATH, then Homebrew's two places. "" when there is none.
pub fn find_adb(environ: Option<&Environ>) -> String {
    let override_ = env_get(environ, "EVE_ADB");
    let override_ = override_.trim();
    if !override_.is_empty() {
        return override_.to_string();
    }
    if let Ok(found) = which::which("adb") {
        return found.to_string_lossy().into_owned();
    }
    ADB_PATHS.iter().find(|p| executable(p)).map(|p| p.to_string()).unwrap_or_default()
}

fn strerror(e: &io::Error) -> String {
    match e.raw_os_error() {
        Some(code) => {
            // SAFETY: strerror returns a pointer to a static (or thread-local) NUL-terminated message.
            let msg = unsafe { std::ffi::CStr::from_ptr(libc::strerror(code)) };
            msg.to_string_lossy().into_owned()
        }
        None => e.to_string(),
    }
}

fn could_not_start(e: &io::Error) -> Vec<u8> {
    format!("adb could not start ({})", strerror(e)).into_bytes()
}

/// The child, killed and reaped if the call is dropped before it finishes: cancelled (the owner talked over a slow
/// voice tool, a Telegram STOP), an `adb connect` to a TV in deep standby would run on for its whole TCP timeout,
/// and a key press would still land after eve moved on.
struct Reaper(Option<tokio::process::Child>);

impl Drop for Reaper {
    fn drop(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.start_kill();
            let deadline = Instant::now() + Duration::from_secs(2);
            while Instant::now() < deadline {
                match child.try_wait() {
                    Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                    _ => break,
                }
            }
        }
    }
}

fn exit_code(status: std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.code().unwrap_or_else(|| -status.signal().unwrap_or(0))
}

async fn kill(reaper: &mut Reaper) {
    if let Some(mut child) = reaper.0.take() {
        let _ = child.start_kill();
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    }
}

/// One process, an argv list (never a shell string), a timeout. (rc, stdout, stderr); with `merge` stderr goes into
/// stdout's pipe, in the order it was written.
async fn spawn(argv: &[String], timeout: f64, merge: bool) -> (i32, Vec<u8>, Vec<u8>) {
    let Some((prog, args)) = argv.split_first() else {
        let e = io::Error::from_raw_os_error(libc::ENOENT);
        return (RC_NO_ADB, Vec::new(), could_not_start(&e));
    };
    let mut cmd = tokio::process::Command::new(prog);
    cmd.args(args).stdin(Stdio::null());
    let mut merged = None;
    if merge {
        let (reader, writer) = match io::pipe() {
            Ok(p) => p,
            Err(e) => return (RC_NO_ADB, Vec::new(), could_not_start(&e)),
        };
        let writer2 = match writer.try_clone() {
            Ok(w) => w,
            Err(e) => return (RC_NO_ADB, Vec::new(), could_not_start(&e)),
        };
        cmd.stdout(Stdio::from(writer)).stderr(Stdio::from(writer2));
        merged = Some(reader);
    } else {
        cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    }
    let child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return (RC_NO_ADB, Vec::new(), could_not_start(&e)),
    };
    drop(cmd); // our copies of the pipe's write end: the reader sees EOF when the child is done
    let mut reaper = Reaper(Some(child));
    let child = reaper.0.as_mut().expect("child just stored");
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let merged = match merged {
        Some(r) => match tokio::net::unix::pipe::Receiver::from_owned_fd(std::os::fd::OwnedFd::from(r)) {
            Ok(rx) => Some(rx),
            Err(e) => {
                kill(&mut reaper).await;
                return (RC_NO_ADB, Vec::new(), could_not_start(&e));
            }
        },
        None => None,
    };
    let work = async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let read_out = async {
            if let Some(mut rx) = merged {
                let _ = rx.read_to_end(&mut out).await;
            } else if let Some(mut so) = stdout {
                let _ = so.read_to_end(&mut out).await;
            }
        };
        let read_err = async {
            if let Some(mut se) = stderr {
                let _ = se.read_to_end(&mut err).await;
            }
        };
        let child = reaper.0.as_mut().expect("child is held until the call ends");
        let (_, _, status) = tokio::join!(read_out, read_err, child.wait());
        (status, out, err)
    };
    match tokio::time::timeout(secs(timeout), work).await {
        Ok((status, out, err)) => {
            let rc = match status {
                Ok(s) => exit_code(s),
                Err(_) => 0,
            };
            reaper.0 = None; // done and reaped by wait()
            (rc, out, err)
        }
        Err(_) => {
            kill(&mut reaper).await;
            (RC_TIMEOUT, Vec::new(), Vec::new())
        }
    }
}

/// The real runner: one adb process, an argv list (never a shell string), a timeout.
pub async fn adb_runner(argv: Vec<String>, timeout: f64) -> (i32, String) {
    let (rc, mut out, err) = spawn(&argv, timeout, true).await;
    out.extend_from_slice(&err);
    (rc, String::from_utf8_lossy(&out).into_owned())
}

/// Like [`adb_runner`] but keeps stdout as bytes; on a failure the error words come back instead.
pub async fn adb_capture(argv: Vec<String>, timeout: f64) -> (i32, Vec<u8>) {
    let (rc, out, mut err) = spawn(&argv, timeout, false).await;
    if rc == 0 {
        (rc, out)
    } else {
        err.extend_from_slice(&out);
        (rc, err)
    }
}

/// [`adb_runner`] as a [`Runner`].
#[derive(Debug, Clone, Copy, Default)]
pub struct AdbRunner;

impl Runner for AdbRunner {
    fn run(&self, argv: Vec<String>, timeout: f64) -> BoxFuture<'_, (i32, String)> {
        Box::pin(adb_runner(argv, timeout))
    }
}

/// [`adb_capture`] as a [`Capture`].
#[derive(Debug, Clone, Copy, Default)]
pub struct AdbCapture;

impl Capture for AdbCapture {
    fn capture(&self, argv: Vec<String>, timeout: f64) -> BoxFuture<'_, (i32, Vec<u8>)> {
        Box::pin(adb_capture(argv, timeout))
    }
}

/// `shlex.quote`: one word for a POSIX shell.
pub fn shlex_quote(s: &str) -> String {
    if s.is_empty() {
        return "''".to_string();
    }
    let safe = s.chars().all(|c| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c));
    if safe {
        return s.to_string();
    }
    format!("'{}'", s.replace('\'', "'\"'\"'"))
}

/// One command line for the TV's shell, every word quoted for it.
fn sh(words: &[&str]) -> String {
    words.iter().map(|w| shlex_quote(w)).collect::<Vec<_>>().join(" ")
}

// ---- cleaning what the TV says -------------------------------------------------------------------------------

static CONTROL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[\x00-\x1f\x7f-\x9f\x{200b}-\x{200f}\x{2028}-\x{202e}\x{2066}-\x{2069}]+").expect("control regex")
});
static SPACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+").expect("spaces regex"));

/// Python's `str.isspace` for one character (`\s` and `strip()` in the Python use it).
fn is_py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

fn py_strip(s: &str) -> &str {
    s.trim_matches(is_py_space)
}

/// A string from the TV, safe for a model or a log: no control characters or newlines, one line, capped.
pub fn clean(text: &str, cap: usize) -> String {
    let s = CONTROL.replace_all(text, " ");
    let s = SPACES.replace_all(&s, " ");
    let s = py_strip(&s);
    if s.chars().count() <= cap {
        return s.to_string();
    }
    let head: String = s.chars().take(cap.saturating_sub(1)).collect();
    format!("{}…", head.trim_end_matches(is_py_space))
}

// ---- the config ----------------------------------------------------------------------------------------------

/// `Path.expanduser`: a leading `~` is the home directory (`$HOME`).
pub fn expanduser(p: &Path) -> PathBuf {
    let s = p.to_string_lossy();
    if s == "~" || s.starts_with("~/") {
        let home = std::env::var_os("HOME").map(PathBuf::from).or_else(dirs::home_dir).unwrap_or_default();
        if s == "~" {
            return home;
        }
        return home.join(&s[2..]);
    }
    p.to_path_buf()
}

fn cfg_path(path: Option<&Path>) -> PathBuf {
    expanduser(path.unwrap_or(Path::new(CONFIG_PATH)))
}

fn port_of(cfg: &Config) -> i64 {
    match cfg.get("port") {
        Some(v) if py_truthy(v) => py_int(v).unwrap_or(DEFAULT_PORT),
        _ => DEFAULT_PORT,
    }
}

fn host_of(cfg: &Config) -> String {
    cfg.get("host").map(py_str).unwrap_or_default()
}

/// The paired TV, or an empty map when there is no file, it is not JSON, or it names no host.
pub fn load(path: Option<&Path>) -> Config {
    let p = cfg_path(path);
    let Ok(text) = std::fs::read_to_string(&p) else {
        return Config::new();
    };
    let Ok(Value::Object(mut d)) = serde_json::from_str::<Value>(&text) else {
        return Config::new();
    };
    let host = match d.get("host") {
        Some(v) if py_truthy(v) => py_str(v),
        _ => String::new(),
    };
    let host = py_strip(&host).to_string();
    if host.is_empty() {
        return Config::new();
    }
    let port = port_of(&d);
    let mdns = match d.get("mdns") {
        Some(v) if py_truthy(v) => py_str(v),
        _ => String::new(),
    };
    d.insert("host".into(), Value::String(host));
    d.insert("port".into(), Value::from(port));
    d.insert("mdns".into(), Value::String(mdns));
    d
}

/// Written private (600) from the first byte: the TV's address and serial are the owner's home.
pub fn save(cfg: &Config, path: Option<&Path>) -> io::Result<PathBuf> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let p = cfg_path(path);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp = p.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
    f.write_all(pyfmt::json_dumps_indent(&Value::Object(cfg.clone()), 2).as_bytes())?;
    drop(f);
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

// ---- the apps ------------------------------------------------------------------------------------------------

/// An app on the TV: the name said, other names for it, and the exact activity that opens it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct App {
    pub name: &'static str,
    pub component: &'static str,
    pub aliases: &'static [&'static str],
    /// Opened by a VIEW link rather than as a launcher (Live TV's guide).
    pub data: &'static str,
}

impl App {
    const fn new(name: &'static str, component: &'static str, aliases: &'static [&'static str]) -> Self {
        App { name, component, aliases, data: "" }
    }

    pub fn package(&self) -> &'static str {
        self.component.split_once('/').map_or(self.component, |(p, _)| p)
    }
}

/// The components were read off the owner's TV on 2026-10-04 (LEANBACK_LAUNCHER and LAUNCHER queries). Amazon
/// renames activities now and then (Prime Video's did in 2024), so a failed start re-resolves the package.
pub static APPS: [App; 18] = [
    App::new("YouTube", "com.amazon.firetv.youtube/dev.cobalt.app.MainActivity", &["you tube", "yt"]),
    App::new("Netflix", "com.netflix.ninja/.MainActivity", &[]),
    App::new(
        "Prime Video",
        "com.amazon.firebat/com.amazon.firebatcore.deeplink.DeepLinkRoutingActivity",
        &["prime", "amazon prime", "amazon prime video", "amazon video"],
    ),
    App::new("Disney+", "com.disney.disneyplus/com.bamtechmedia.dominguez.main.MainActivity", &["disney", "disney plus"]),
    App::new("Hulu", "com.hulu.plus/.SplashActivity", &[]),
    App::new("Peacock", "com.peacock.peacockfiretv/com.peacock.peacocktv.AmazonMainActivity", &[]),
    App::new("Spotify", "com.spotify.tv.android/.SpotifyTVActivity", &[]),
    App::new("ESPN", "com.espn.gtv/com.espn.startup.presentation.StartupActivity", &[]),
    App::new(
        "NBC Sports",
        "com.nbcuni.com.nbcsports.liveextra.firetv/com.nbc.nbctvapp.ui.main.view.MainActivity",
        &["nbc"],
    ),
    App::new("NFL", "com.gotv.nflgamecenter.us.lite/com.nfl.connected.SplashActivity", &["nfl app"]),
    App::new("NBA", "com.nbaimd.gametime.nba2011.amazon/com.nba.tv.ui.splash.SplashActivity", &[]),
    App::new("Fox Sports", "com.fox.fts.android/.MainActivity", &["fox"]),
    App::new("Amazon Music", "com.amazon.bueller.music/com.amazon.music.MainActivity", &[]),
    App::new("Photos", "com.amazon.bueller.photos/.StubActivity", &["amazon photos"]),
    App::new("Silk", "com.amazon.cloud9/.browsing.BrowserActivity", &["silk browser", "browser", "web browser"]),
    App::new(
        "Appstore",
        "com.amazon.venezia/.grid.AppsGridLauncherActivity",
        &["app store", "apps", "my apps", "amazon appstore"],
    ),
    App::new("Settings", "com.amazon.tv.launcher/.ui.SettingsActivity", &["tv settings"]),
    App {
        name: "Live TV",
        component: "com.amazon.tv.livetv/.GuideWrapperActivity",
        aliases: &["live", "guide", "tv guide", "channel guide", "channels", "antenna"],
        data: "livetv://showGuideButtonContent?source=UNKNOWN",
    },
];
pub static YOUTUBE: &App = &APPS[0];
pub const LAUNCHER_PACKAGE: &str = "com.amazon.tv.launcher";
/// A third-party launcher the Home button may open instead.
pub const PROJECTIVY_PACKAGE: &str = "com.spocky.projengmenu";
pub const SCREENSAVER_PACKAGE: &str = "com.amazon.ftv.screensaver";
/// The TV showing an HDMI input.
pub const PASSTHROUGH_ACTIVITY: &str = "PassthroughPlayerActivity";

static NOT_APP_CHARS: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^a-z0-9 ]+").expect("app key regex"));
static LEADING_ARTICLE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:the|my)\s+").expect("article regex"));
static TRAILING_APP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s+app$").expect("app suffix regex"));

fn app_key(text: &str) -> String {
    let t = text.to_lowercase().replace('+', " plus ").replace('\u{2019}', "'");
    let t = NOT_APP_CHARS.replace_all(&t, " ");
    let t = SPACES.replace_all(&t, " ");
    let t = py_strip(&t);
    let t = LEADING_ARTICLE.replace(t, "");
    TRAILING_APP.replace(&t, "").into_owned()
}

static BY_ALIAS: LazyLock<HashMap<String, &'static App>> = LazyLock::new(|| {
    let mut m = HashMap::new();
    for a in APPS.iter() {
        for k in std::iter::once(&a.name).chain(a.aliases.iter()) {
            m.entry(app_key(k)).or_insert(a);
        }
    }
    m
});

pub fn find_app(name: &str) -> Option<&'static App> {
    BY_ALIAS.get(&app_key(name)).copied()
}

/// The foreground app in words: the table's name, "home", "screensaver", "an HDMI input", or the package.
pub fn app_name(package: &str, activity: &str) -> String {
    if package.is_empty() {
        return String::new();
    }
    if activity.contains(PASSTHROUGH_ACTIVITY) {
        return "an HDMI input".into();
    }
    if package == SCREENSAVER_PACKAGE {
        return "screensaver".into();
    }
    if package == LAUNCHER_PACKAGE {
        return if activity.contains("SettingsActivity") { "Settings" } else { "home" }.into();
    }
    if package == PROJECTIVY_PACKAGE {
        return if activity.contains(".ui.home.") { "home" } else { "Projectivy settings" }.into();
    }
    if let Some(a) = APPS.iter().find(|a| a.package() == package) {
        return a.name.into();
    }
    clean(package, 80)
}

// ---- the TV's state, in one round trip -----------------------------------------------------------------------

/// Read-only, one `adb shell`. Never `grep -m1` on the TV: closing the pipe early breaks dumpsys. The awk keeps
/// reading to the end for the same reason; it prints only the STREAM_MUSIC block.
pub const STATE_SCRIPT: &str = "echo @@POWER; dumpsys power | grep -E 'mWakefulness=|Display Power: state='; \
                                echo @@FOCUS; dumpsys window | grep mCurrentFocus || dumpsys activity activities \
                                | grep mResumedActivity; \
                                echo @@AUDIO; dumpsys audio | awk '/^- STREAM_/{f=($0 ~ /STREAM_MUSIC:/)} f'; \
                                echo @@END";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TvState {
    /// on, screensaver, standby, unknown
    pub power: String,
    pub wakefulness: String,
    pub display: String,
    pub package: String,
    pub activity: String,
    pub app: String,
    /// STREAM_MUSIC's index, on the TV's own scale.
    pub index: Option<i64>,
    pub max_index: Option<i64>,
    pub muted: Option<bool>,
}

impl Default for TvState {
    fn default() -> Self {
        TvState {
            power: "unknown".into(),
            wakefulness: String::new(),
            display: String::new(),
            package: String::new(),
            activity: String::new(),
            app: String::new(),
            index: None,
            max_index: None,
            muted: None,
        }
    }
}

impl TvState {
    /// The volume in percent, or None when it could not be read.
    pub fn volume(&self) -> Option<i64> {
        let index = self.index?;
        let max = self.max_index.filter(|m| *m != 0)?;
        Some(py_round(index as f64 * 100.0 / max as f64))
    }

    pub fn public(&self) -> Out {
        let mut m = Out::new();
        m.insert("power".into(), Value::String(self.power.clone()));
        m.insert("app".into(), if self.app.is_empty() { Value::Null } else { Value::String(self.app.clone()) });
        m.insert("volume".into(), self.volume().map_or(Value::Null, Value::from));
        m.insert("muted".into(), self.muted.map_or(Value::Null, Value::Bool));
        m
    }
}

/// `re.search(rf"@@{name}\s*\n(.*?)(?=\n?@@[A-Z]+|\Z)", text, re.S)`, group 1, or "".
fn section<'a>(text: &'a str, name: &str) -> &'a str {
    let tag = format!("@@{name}");
    let mut from = 0;
    while let Some(i) = text[from..].find(&tag) {
        let after = from + i + tag.len();
        let rest = &text[after..];
        let run = rest.char_indices().find(|(_, c)| !is_py_space(*c)).map_or(rest.len(), |(j, _)| j);
        if let Some(nl) = rest[..run].rfind('\n') {
            let body = &rest[nl + 1..];
            let bytes = body.as_bytes();
            let mut end = body.len();
            let mut q = 0;
            while let Some(k) = body[q..].find("@@") {
                let at = q + k;
                if bytes.get(at + 2).is_some_and(|b| b.is_ascii_uppercase()) {
                    end = if at > 0 && bytes[at - 1] == b'\n' { at - 1 } else { at };
                    break;
                }
                q = at + 1;
            }
            return &body[..end];
        }
        from = from + i + 1;
    }
    ""
}

/// Awake + screen on = on; Dreaming = the screensaver (still on); Asleep, Dozing or screen off = standby.
pub fn power_word(wakefulness: &str, display: &str) -> String {
    let w = wakefulness.to_lowercase();
    let d = display.to_uppercase();
    if w == "dreaming" {
        return "screensaver".into();
    }
    if w == "asleep" || w == "dozing" || d == "OFF" {
        return "standby".into();
    }
    if w == "awake" {
        return "on".into();
    }
    "unknown".into()
}

macro_rules! re {
    ($name:ident, $pat:expr) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| Regex::new($pat).expect(stringify!($name)));
    };
}

re!(WAKE_RE, r"mWakefulness=(\w+)");
re!(DISPLAY_RE, r"Display Power: state=(\w+)");
re!(FOCUS_RE, r" u0 ([A-Za-z0-9_.]+)(?:/([A-Za-z0-9_.$]+))?");
re!(VOLUME_RE, r"streamVolume:\s*(\d+)");
re!(MAX_RE, r"\bMax:\s*(\d+)");
re!(MUTED_RE, r"\bMuted:\s*(true|false)");

fn group1(re: &Regex, text: &str) -> Option<String> {
    re.captures(text).and_then(|c| c.get(1)).map(|m| m.as_str().to_string())
}

pub fn parse_state(text: &str) -> TvState {
    let power = section(text, "POWER");
    let w = group1(&WAKE_RE, power).unwrap_or_default();
    let d = group1(&DISPLAY_RE, power).unwrap_or_default();
    let focus = section(text, "FOCUS");
    let (mut pkg, mut act) = (String::new(), String::new());
    if let Some(m) = FOCUS_RE.captures(focus) {
        pkg = m.get(1).map_or("", |g| g.as_str()).to_string();
        act = m.get(2).map_or("", |g| g.as_str()).to_string();
        if act.starts_with('.') {
            act = format!("{pkg}{act}");
        }
    }
    let audio = section(text, "AUDIO");
    let vol = group1(&VOLUME_RE, audio).and_then(|v| pyfmt::parse_decimal(&v));
    let mx = group1(&MAX_RE, audio).and_then(|v| pyfmt::parse_decimal(&v)).filter(|m| *m > 0);
    let muted = group1(&MUTED_RE, audio).map(|m| m == "true");
    TvState {
        power: power_word(&w, &d),
        app: app_name(&pkg, &act),
        wakefulness: w,
        display: d,
        package: pkg,
        activity: act,
        index: vol,
        max_index: mx,
        muted,
    }
}

/// 'TV on — YouTube, volume 44%'.
pub fn said_state(st: &TvState) -> String {
    let vol = match st.volume() {
        None => "volume unknown".to_string(),
        Some(v) => format!("volume {v}%") + if st.muted == Some(true) { " (muted)" } else { "" },
    };
    if st.power == "standby" {
        return "TV in standby".to_string() + &if st.volume().is_none() { ".".to_string() } else { format!(", {vol}.") };
    }
    if st.power == "unknown" {
        return format!("TV state unknown, {vol}.");
    }
    let what = if !st.app.is_empty() {
        st.app.as_str()
    } else if st.power == "screensaver" {
        "screensaver"
    } else {
        ""
    };
    let what = if what.is_empty() { String::new() } else { format!(" — {what}") };
    format!("TV on{what}, {vol}")
}

// ---- the keys ------------------------------------------------------------------------------------------------

pub const NAV_KEYS: [(&str, i64); 8] =
    [("up", 19), ("down", 20), ("left", 21), ("right", 22), ("select", 23), ("back", 4), ("home", 3), ("menu", 82)];
pub const KEY_ALIASES: [(&str, &str); 4] =
    [("ok", "select"), ("enter", "select"), ("center", "select"), ("centre", "select")];
/// `cmd media_session dispatch` names (AOSP MediaShellCommand.runDispatch). `stop` is for the terminal only.
pub const MEDIA_KEYS: [(&str, &str); 8] = [
    ("play_pause", "play-pause"),
    ("play", "play"),
    ("pause", "pause"),
    ("next", "next"),
    ("previous", "previous"),
    ("rewind", "rewind"),
    ("fast_forward", "fast-forward"),
    ("stop", "stop"),
];
pub const KEYCODE_WAKEUP: i64 = 224;
pub const KEYCODE_SLEEP: i64 = 223;
pub const KEYCODE_VOLUME_MUTE: i64 = 164;

pub fn nav_key(name: &str) -> Option<i64> {
    NAV_KEYS.iter().find(|(k, _)| *k == name).map(|(_, c)| *c)
}

pub fn media_key(name: &str) -> Option<&'static str> {
    MEDIA_KEYS.iter().find(|(k, _)| *k == name).map(|(_, v)| *v)
}

/// `KEY_ALIASES.get(key, key)`.
pub fn key_alias(key: &str) -> &str {
    KEY_ALIASES.iter().find(|(k, _)| *k == key).map_or(key, |(_, v)| v)
}

// ---- the tools -----------------------------------------------------------------------------------------------

pub const ACTIONS: [&str; 23] = [
    "power_on", "power_off", "up", "down", "left", "right", "select", "back", "home", "menu", "play_pause", "play",
    "pause", "next", "previous", "rewind", "fast_forward", "volume", "mute", "unmute", "open_app", "youtube_search",
    "type_text",
];
/// What the Mini App's remote card sends ([`FireTV::panel`]): the buttons on a remote, the volume and the app
/// chips. Typing and YouTube search stay with the chat.
pub const PANEL_ACTIONS: [&str; 18] = [
    "power_on", "power_off", "up", "down", "left", "right", "select", "back", "home", "menu", "play_pause", "rewind",
    "fast_forward", "volume", "volume_change", "mute", "unmute", "open_app",
];

pub static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    vec![
        json!({"type": "function", "name": "firetv_status", "strict": true,
               "description": "The owner's Fire TV right now: power (on, screensaver, standby), the app in front, \
                               the volume in percent and whether it is muted.",
               "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}}}),
        json!({"type": "function", "name": "firetv_control", "strict": true,
               "description": "Control the owner's Fire TV: power_on / power_off; the remote's d-pad (up, down, \
                               left, right, select, back, home, menu, with repeat); media keys (play_pause, play, \
                               pause, next, previous, rewind, fast_forward); volume (a level or a change), mute, \
                               unmute; open_app by name; youtube_search; type_text into the box the TV has \
                               focused. Pass null for what you don't use.",
               "parameters": {"type": "object", "additionalProperties": false,
                              "required": ["action", "app", "query", "text", "volume", "volume_change", "repeat"],
                              "properties": {
                                  "action": {"type": "string", "enum": ACTIONS},
                                  "app": {"type": ["string", "null"],
                                          "description": "open_app: the app's name (\"YouTube\", \"Netflix\", \
                                                          \"Prime Video\"); or null."},
                                  "query": {"type": ["string", "null"],
                                            "description": "youtube_search: what to search for; or null."},
                                  "text": {"type": ["string", "null"],
                                           "description": "type_text: plain ASCII, up to 200 characters; or null."},
                                  "volume": {"type": ["integer", "null"],
                                             "description": "volume: 0-100 percent; or null."},
                                  "volume_change": {"type": ["integer", "null"],
                                                    "description": "volume: a change such as 10 or -10, when no \
                                                                    level was asked; or null."},
                                  "repeat": {"type": ["integer", "null"],
                                             "description": "d-pad keys: how many presses, 1-10; or null for one."}}}}),
        json!({"type": "function", "name": "firetv_screenshot", "strict": true,
               "description": "Send the owner a picture of what their Fire TV shows now. You don't see the \
                               picture: you get back the TV's state at the same moment (power, the app in front, \
                               the volume). Use it when the owner wants to see the screen, or to check where \
                               navigating landed.",
               "parameters": {"type": "object", "additionalProperties": false, "required": [], "properties": {}}}),
    ]
});
pub const TOOL_NAMES: [&str; 3] = ["firetv_status", "firetv_control", "firetv_screenshot"];

// The model's guide. The screenshot sentences are left out where there is no screenshot tool (the live voice).
const SHOT_TOOL: &str =
    " firetv_screenshot sends the owner a picture of the screen; you don't see it, only the state it returns.";
const SHOT_CHECK: &str = "; send a screenshot when the owner needs to see where it landed";
const INSTRUCTIONS_TEMPLATE: &str = "The owner's Fire TV.
What you can do: firetv_control turns it on or off, presses the remote's keys, plays and pauses, sets the volume,
mutes, opens an app by name, searches YouTube and types into a focused search box. firetv_status reads power, the
app in front and the volume.{shot_tool} \"Turn it up\" is volume_change 10, \"a bit\" 5.
Verify: power, volume and mute are read back, so say the result you got. The d-pad, media keys, opening apps and
typing are not, so say what you pressed or opened{shot_check}. Don't guess what is on the screen.
When an app is already in front and the owner asks for another, go ahead, but say what you interrupted.
Limits: HDMI inputs can't be switched. In deep standby the TV can't be reached: say to use the remote or Alexa.
Search works in YouTube only.";

fn instructions_with(shot_tool: &str, shot_check: &str) -> String {
    INSTRUCTIONS_TEMPLATE.replace("{shot_tool}", shot_tool).replace("{shot_check}", shot_check)
}

pub static INSTRUCTIONS: LazyLock<String> = LazyLock::new(|| instructions_with(SHOT_TOOL, SHOT_CHECK));
pub static INSTRUCTIONS_NO_SCREENSHOT: LazyLock<String> = LazyLock::new(|| instructions_with("", ""));

// ---- the code path -------------------------------------------------------------------------------------------

/// A TV command the code understood.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    /// status, power_on, power_off, a key, volume, mute, unmute, open_app, ...
    pub action: String,
    pub app: String,
    pub query: String,
    pub text: String,
    pub volume: Option<i64>,
    pub change: Option<i64>,
    pub repeat: i64,
}

impl Command {
    pub fn new(action: &str) -> Self {
        Command {
            action: action.to_string(),
            app: String::new(),
            query: String::new(),
            text: String::new(),
            volume: None,
            change: None,
            repeat: 1,
        }
    }

    pub fn with_app(mut self, app: &str) -> Self {
        self.app = app.to_string();
        self
    }

    pub fn with_query(mut self, query: &str) -> Self {
        self.query = query.to_string();
        self
    }

    pub fn with_volume(mut self, volume: i64) -> Self {
        self.volume = Some(volume);
        self
    }

    pub fn with_change(mut self, change: i64) -> Self {
        self.change = Some(change);
        self
    }

    pub fn with_repeat(mut self, repeat: i64) -> Self {
        self.repeat = repeat;
        self
    }
}

const LEAD: &str = r"(?:(?:hey|ok|okay)\s+)?(?:eve\s+)?(?:(?:please|pls|can you|could you|would you)\s+)?";
const TAIL: &str = r"(?:\s+(?:please|pls|thanks|thank you|eve|now))?";
const TV: &str = r"(?:the\s+|my\s+)?(?:fire\s?tv|tv|television|telly)";
// "what's on tv" with no article asks about the listings, not this TV: that form needs "the", "my" or "fire tv"
const THIS_TV: &str = r"(?:(?:the|my)\s+(?:fire\s?tv|tv|television|telly)|fire\s?tv)";
// Spotify owns "play the music on the tv" (a Connect transfer to the TV's Spotify): those words are left to it.
const MUSIC: &str = r"(?:the\s+|my\s+)?(?:music|spotify|song|track|playback|tunes)\b";
const APP_RE: &str = r"([a-z0-9][a-z0-9' -]{0,38}?)";
const BIT: &str = r"(\s+a\s+(?:bit|little|touch))?";
const KEY: &str = r"(up|down|left|right|select|ok|enter|back|home|menu)";
const TIMES: &str = r"(?:\s+(\d{1,2}|one|two|three|four|five|six|seven|eight|nine|ten)\s+times|\s+(once|twice))?";
const UP: &str = r"(?:up|louder)";
const DOWN: &str = r"(?:down|quieter|softer)";

fn count_word(g: &str) -> i64 {
    match g {
        "once" | "one" => 1,
        "two" | "twice" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        _ => 1,
    }
}

type Caps<'t> = fancy_regex::Captures<'t, str>;

fn group<'t>(m: &Caps<'t>, i: usize) -> Option<&'t str> {
    m.get(i).map(|g| g.as_str()).filter(|s| !s.is_empty())
}

fn count(groups: &[Option<&str>]) -> i64 {
    match groups.iter().flatten().next() {
        None => 1,
        Some(g) if pyfmt::parse_decimal(g).is_some() => pyfmt::parse_decimal(g).unwrap_or(1),
        Some(g) => count_word(g),
    }
}

fn key_cmd(m: &Caps<'_>) -> Command {
    let k = group(m, 1).unwrap_or("");
    Command::new(key_alias(k)).with_repeat(count(&[group(m, 2), group(m, 3)]))
}

fn step(m: &Caps<'_>, sign: i64) -> Command {
    let bit = (1..m.len()).filter_map(|i| group(m, i)).any(|g| py_strip(g).starts_with("a "));
    Command::new("volume").with_change(sign * if bit { VOLUME_STEP / 2 } else { VOLUME_STEP })
}

type Maker = fn(&Caps<'_>) -> Option<Command>;

fn first_group<'t>(m: &Caps<'t>) -> Option<&'t str> {
    (1..m.len()).find_map(|i| group(m, i))
}

static PATTERNS: LazyLock<Vec<(fancy_regex::Regex, Maker)>> = LazyLock::new(|| {
    let tv = TV;
    let raw: Vec<(String, Maker)> = vec![
        (
            format!(
                r"(?:turn|switch|power)\s+on\s+{tv}|(?:turn|switch|power)\s+{tv}\s+on|{tv}\s+on|wake\s+(?:up\s+)?{tv}(?:\s+up)?"
            ),
            |_| Some(Command::new("power_on")),
        ),
        (
            format!(
                r"(?:turn|switch|power)\s+off\s+{tv}|(?:turn|switch|power)\s+{tv}\s+off|{tv}\s+off|put\s+{tv}\s+to\s+sleep"
            ),
            |_| Some(Command::new("power_off")),
        ),
        (
            format!(
                r"(?:what'?s|what\s+is)\s+(?:on|playing\s+on)\s+{THIS_TV}|is\s+{tv}\s+on|{tv}\s+status|(?:what'?s|what\s+is)\s+{tv}\s+(?:showing|doing|playing)"
            ),
            |_| Some(Command::new("status")),
        ),
        (
            format!(
                r"(?:set\s+)?{tv}\s+volume\s+(?:to\s+)?(\d{{1,3}})\s*%?|set\s+(?:the\s+)?volume\s+(?:on|of)\s+{tv}\s+to\s+(\d{{1,3}})\s*%?|(?:set|turn)\s+{tv}\s+(?:volume\s+)?to\s+(\d{{1,3}})\s*%?"
            ),
            |m| first_group(m).and_then(pyfmt::parse_decimal).map(|v| Command::new("volume").with_volume(v)),
        ),
        (
            format!(
                r"{tv}\s+volume\s+{UP}{BIT}|turn\s+{tv}\s+{UP}{BIT}|turn\s+up\s+{tv}{BIT}|{tv}\s+louder{BIT}|turn\s+(?:the\s+)?volume\s+up\s+on\s+{tv}{BIT}|turn\s+up\s+the\s+volume\s+on\s+{tv}{BIT}|louder\s+on\s+{tv}"
            ),
            |m| Some(step(m, 1)),
        ),
        (
            format!(
                r"{tv}\s+volume\s+{DOWN}{BIT}|turn\s+{tv}\s+{DOWN}{BIT}|turn\s+down\s+{tv}{BIT}|{tv}\s+(?:quieter|softer){BIT}|turn\s+(?:the\s+)?volume\s+down\s+on\s+{tv}{BIT}|turn\s+down\s+the\s+volume\s+on\s+{tv}{BIT}|quieter\s+on\s+{tv}"
            ),
            |m| Some(step(m, -1)),
        ),
        (format!(r"mute\s+{tv}|{tv}\s+mute"), |_| Some(Command::new("mute"))),
        (format!(r"unmute\s+{tv}|{tv}\s+unmute"), |_| Some(Command::new("unmute"))),
        (format!(r"pause\s+{tv}|{tv}\s+pause|pause\s+(?:it\s+)?on\s+{tv}"), |_| Some(Command::new("pause"))),
        (
            format!(r"(?:play|resume|unpause)\s+{tv}|{tv}\s+(?:play|resume)|(?:resume|unpause)\s+on\s+{tv}"),
            |_| Some(Command::new("play")),
        ),
        (
            format!(r"(?:go\s+(?:back\s+)?)?home\s+on\s+{tv}|go\s+to\s+(?:the\s+)?home\s+screen\s+on\s+{tv}"),
            |_| Some(Command::new("home")),
        ),
        // "tv up" alone most likely means louder, so a bare up or down needs a count ("tv down 3 times") to be the
        // d-pad; without one the model hears it
        (format!(r"{tv}\s+{KEY}{TIMES}"), |m| {
            let k = group(m, 1).unwrap_or("");
            if (k == "up" || k == "down") && group(m, 2).is_none() && group(m, 3).is_none() {
                None
            } else {
                Some(key_cmd(m))
            }
        }),
        (format!(r"press\s+{KEY}{TIMES}\s+on\s+{tv}"), |m| Some(key_cmd(m))),
        (
            format!(
                r"(?:play|search(?:\s+for)?|find|look\s+up|put\s+on)\s+(.{{1,80}}?)\s+on\s+youtube\s+on\s+{tv}|search\s+youtube\s+for\s+(.{{1,80}}?)\s+on\s+{tv}"
            ),
            |m| first_group(m).map(|q| Command::new("youtube_search").with_query(py_strip(q))),
        ),
        // "open" names an app outright, so any name goes (an unknown one gets a plain "no such app")
        (
            format!(
                r"(?:open|launch|start|load)\s+(?:up\s+)?(?!(?:the\s+|my\s+)?(?:music|song|track|playback|tunes)\b){APP_RE}(?:\s+app)?\s+on\s+{tv}"
            ),
            |m| Some(Command::new("open_app").with_app(py_strip(m.get(1).map_or("", |g| g.as_str())))),
        ),
        // "put/play/watch X on the tv" opens X only when X is an app eve knows; "play daft punk on the tv" is not
        (
            format!(r"(?:put|play|watch|switch\s+to|go\s+to)\s+(?:on\s+)?(?!{MUSIC}){APP_RE}(?:\s+app)?\s+on\s+{tv}"),
            |m| {
                let name = m.get(1).map_or("", |g| g.as_str());
                find_app(name).map(|_| Command::new("open_app").with_app(py_strip(name)))
            },
        ),
    ];
    raw.into_iter()
        .map(|(p, make)| {
            // the lead and the tail only have non-capturing groups: the pattern's own groups come through as they are
            let full = format!("^{LEAD}(?:{p}){TAIL}$");
            (fancy_regex::Regex::new(&full).expect("firetv command pattern"), make)
        })
        .collect()
});

static NORM_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"[^\w%'\s\-]").expect("norm regex"));

fn norm(text: &str) -> String {
    let t = text.to_lowercase().replace('\u{2019}', "'").replace('+', " plus ");
    let t = NORM_RE.replace_all(&t, " ");
    let t = SPACES.replace_all(&t, " ");
    py_strip(&t).to_string()
}

/// A Command when the whole message is a TV command that names the TV, else None (the model answers).
/// Anchored at both ends: "turn on the tv in ten minutes" leaves words over, so the model gets it. Reads no
/// state, so telegram can use it to say [`NOT_SET_UP_LINE`] when no TV is paired. (Python: `match`.)
pub fn match_command(text: &str) -> Option<Command> {
    let t = norm(text);
    if t.is_empty() || t.chars().count() > 120 {
        return None;
    }
    for (pattern, make) in PATTERNS.iter() {
        if let Ok(Some(m)) = pattern.captures(t.as_str()) {
            let cmd = make(&m)?;
            if cmd.volume.is_some_and(|v| v > 100) {
                return None;
            }
            return Some(cmd);
        }
    }
    None
}

// ---- the whole thing -----------------------------------------------------------------------------------------

/// Whether a command can be sent: the TV answered and allowed this Mac. `why` names what failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reach {
    pub ok: bool,
    /// no_adb, unreachable, unauthorized, offline, timeout
    pub why: &'static str,
    pub line: &'static str,
}

impl Reach {
    pub const fn ok() -> Self {
        Reach { ok: true, why: "", line: "" }
    }

    pub const fn fail(why: &'static str, line: &'static str) -> Self {
        Reach { ok: false, why, line }
    }
}

// ``get-state`` and ``exec-out`` say "error: ...", but ``adb shell`` says "adb: device '...' not found" (adb 1.0.41,
// checked 2026-10-04): both prefixes are the TV gone, before the command ran
re!(
    GONE_RE,
    r"(?i)(?:error|adb): (?:device (?:offline|'[^']*' not found|not found|still connecting)|closed|no devices(?:/emulators)? found|no devices|protocol fault)"
);

/// adb said the TV is not there: the command never reached it, so sending it again is safe.
pub fn gone(rc: i32, out: &str) -> bool {
    rc != 0 && rc != RC_TIMEOUT && GONE_RE.is_match(out)
}

/// The TV went away under a command (as opposed to the command failing on the TV): gone, or no answer in time.
pub fn lost(rc: i32, out: &str) -> bool {
    rc == RC_TIMEOUT || gone(rc, out)
}

re!(AM_FAILED_RE, r"(?m)^(?:Error|Exception|java\.lang\.\w+Exception)");
re!(RESOLVED_RE, r"(?m)^\s*([A-Za-z0-9_.]+/[A-Za-z0-9_.$]+)\s*$");
re!(PACKAGE_RE, r"^[A-Za-z][A-Za-z0-9_]*(?:\.[A-Za-z0-9_]+)+$");
re!(PM_PACKAGE_RE, r"(?m)^package:([A-Za-z0-9_.]+)\s*$");
re!(SERIAL_RE, r"^[A-Za-z0-9]+$");
re!(PRINTABLE_RE, r"^[ -~]+$");

pub fn am_failed(out: &str) -> bool {
    AM_FAILED_RE.is_match(out)
}

/// The last `package/activity` line of `cmd package resolve-activity --brief`.
pub fn resolved_component(out: &str) -> String {
    RESOLVED_RE
        .captures_iter(out)
        .filter_map(|c| c.get(1).map(|m| m.as_str().to_string()))
        .last()
        .unwrap_or_default()
}

/// (the `input text` argument, "") or ("", why not). Spaces become %s, the way `input text` wants them; the word
/// is shell-quoted when the command line is built.
pub fn type_text_arg(text: &str) -> (String, String) {
    if text.is_empty() {
        return (String::new(), "Nothing to type.".into());
    }
    if text.chars().count() > MAX_TEXT {
        return (String::new(), format!("That's too long to type on the TV (over {MAX_TEXT} characters)."));
    }
    if !PRINTABLE_RE.is_match(text) {
        return (String::new(), "I can only type plain English letters, digits and punctuation on the TV.".into());
    }
    (text.replace(' ', "%s"), String::new())
}

/// `urllib.parse.quote_plus`: spaces become `+`, everything but letters, digits and `_.-~` is %-encoded.
pub fn quote_plus(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        match b {
            b' ' => out.push('+'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_' | b'.' | b'-' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

fn obj(pairs: Vec<(&str, Value)>) -> Out {
    pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
}

/// `{**a, **b}`: a key keeps its first place, takes its last value.
fn merged(mut a: Out, b: Out) -> Out {
    for (k, v) in b {
        a.insert(k, v);
    }
    a
}

fn ok_line(line: impl Into<String>) -> Out {
    obj(vec![("ok", Value::Bool(true)), ("line", Value::String(line.into()))])
}

fn is_ok(out: &Out) -> bool {
    out.get("ok").is_some_and(py_truthy)
}

fn get_str(out: &Out, key: &str) -> String {
    match out.get(key) {
        Some(v) if py_truthy(v) => py_str(v),
        _ => String::new(),
    }
}

/// `num(v)` in `handle` and `panel`: an int or a float (never a bool) as an int, else None.
fn num(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Some(i)
            } else if let Some(u) = n.as_u64() {
                Some(i64::try_from(u).unwrap_or(i64::MAX))
            } else {
                n.as_f64().map(|f| f.trunc() as i64)
            }
        }
        _ => None,
    }
}

/// `str(d.get(key) or default)`.
fn arg_str(d: &Value, key: &str, default: &str) -> String {
    match d.get(key) {
        Some(v) if py_truthy(v) => py_str(v),
        _ => default.to_string(),
    }
}

/// What `control` takes besides the action (Python's keyword arguments).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ControlArgs {
    pub app: String,
    pub query: String,
    pub text: String,
    pub volume: Option<i64>,
    pub volume_change: Option<i64>,
    pub repeat: Option<i64>,
}

/// The owner's Fire TV: the connection, the commands, the tools, the code path.
pub struct FireTV {
    cfg: Mutex<Config>,
    pub runner: Arc<dyn Runner>,
    pub capture: Arc<dyn Capture>,
    pub probe: Arc<dyn Probe>,
    pub adb: String,
    /// Where a rediscovered address is saved; None keeps it in memory.
    pub path: Option<PathBuf>,
    pub shots_dir: Option<PathBuf>,
    sleep: Sleep,
    clock: Clock,
    ready: AtomicBool,
    /// The clock at the last adb server restart.
    restarted_at: Mutex<Option<f64>>,
    lock: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for FireTV {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FireTV").field("serial", &self.serial()).field("adb", &self.adb).finish_non_exhaustive()
    }
}

impl FireTV {
    /// A TV with the real adb runner, capture and probe, `adb` as the program, no saving.
    pub fn new(cfg: Config) -> Self {
        FireTV {
            cfg: Mutex::new(cfg),
            runner: Arc::new(AdbRunner),
            capture: Arc::new(AdbCapture),
            probe: Arc::new(TcpProbe),
            adb: "adb".into(),
            path: None,
            shots_dir: None,
            sleep: default_sleep(),
            clock: default_clock(),
            ready: AtomicBool::new(false),
            restarted_at: Mutex::new(None),
            lock: tokio::sync::Mutex::new(()),
        }
    }

    pub fn with_runner(mut self, runner: Arc<dyn Runner>) -> Self {
        self.runner = runner;
        self
    }

    pub fn with_capture(mut self, capture: Arc<dyn Capture>) -> Self {
        self.capture = capture;
        self
    }

    pub fn with_probe(mut self, probe: Arc<dyn Probe>) -> Self {
        self.probe = probe;
        self
    }

    pub fn with_adb(mut self, adb: &str) -> Self {
        self.adb = adb.to_string();
        self
    }

    pub fn with_path(mut self, path: Option<PathBuf>) -> Self {
        self.path = path;
        self
    }

    pub fn with_shots_dir(mut self, dir: Option<PathBuf>) -> Self {
        self.shots_dir = dir;
        self
    }

    pub fn with_sleep(mut self, sleep: Sleep) -> Self {
        self.sleep = sleep;
        self
    }

    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = clock;
        self
    }

    /// The config as it is now (a rediscovery may have moved the address).
    pub fn cfg(&self) -> Config {
        self.cfg.lock().clone()
    }

    pub fn serial(&self) -> String {
        let cfg = self.cfg.lock();
        format!("{}:{}", host_of(&cfg), port_of(&cfg))
    }

    // -- the connection --
    async fn adb(&self, args: &[&str], timeout: f64) -> (i32, String) {
        let mut argv = Vec::with_capacity(args.len() + 1);
        argv.push(self.adb.clone());
        argv.extend(args.iter().map(|a| a.to_string()));
        self.runner.run(argv, timeout).await
    }

    async fn connect(&self) -> Reach {
        if self.adb.is_empty() {
            return Reach::fail("no_adb", INSTALL_LINE);
        }
        let serial = self.serial();
        let (rc, out) = self.adb(&["connect", &serial], CONNECT_SECS).await;
        let low = out.to_lowercase();
        if rc == RC_NO_ADB {
            return Reach::fail("no_adb", INSTALL_LINE);
        }
        if low.contains("unauthorized") || low.contains("failed to authenticate") {
            return Reach::fail("unauthorized", ALLOW_LINE);
        }
        // adb connect exits 0 either way: its words are the answer
        if rc == RC_TIMEOUT
            || ["failed to connect", "unable to connect", "cannot connect"].iter().any(|w| low.contains(w))
            || !low.contains("connected to")
        {
            return Reach::fail("unreachable", UNREACHABLE_LINE);
        }
        let (rc, out) = self.adb(&["-s", &serial, "get-state"], CONNECT_SECS).await;
        let state = py_strip(&out).to_lowercase();
        if rc == 0 && pyfmt::splitlines(&state).last().is_some_and(|l| *l == "device") {
            self.ready.store(true, Ordering::SeqCst);
            return Reach::ok();
        }
        if state.contains("unauthorized") {
            return Reach::fail("unauthorized", ALLOW_LINE);
        }
        Reach::fail("offline", UNREACHABLE_LINE)
    }

    /// `adb mdns services`: (instance, host, port) of every plain-ADB service on the network.
    pub async fn discover(&self) -> Vec<(String, String, i64)> {
        let (rc, out) = self.adb(&["mdns", "services"], MDNS_SECS).await;
        if rc == 0 {
            parse_mdns(&out)
        } else {
            Vec::new()
        }
    }

    async fn rediscover(&self) {
        let name = {
            let cfg = self.cfg.lock();
            match cfg.get("mdns") {
                Some(v) if py_truthy(v) => py_str(v),
                _ => String::new(),
            }
        };
        if name.is_empty() {
            return;
        }
        for (instance, host, port) in self.discover().await {
            if instance != name {
                continue;
            }
            let snapshot = {
                let mut cfg = self.cfg.lock();
                if (host.as_str(), port) == (host_of(&cfg).as_str(), port_of(&cfg)) {
                    None
                } else {
                    cfg.insert("host".into(), Value::String(host.clone()));
                    cfg.insert("port".into(), Value::from(port));
                    Some(cfg.clone())
                }
            };
            if let Some(cfg) = snapshot {
                tracing::info!("firetv: found the TV at a new address by its mDNS name");
                if let Some(path) = &self.path {
                    if let Err(e) = save(&cfg, Some(path)) {
                        tracing::warn!("firetv: could not keep the new address ({})", e);
                    }
                }
            }
            return;
        }
    }

    /// Once: drop the old connection, look the TV up by name, connect again; then, only if the TV's port opens
    /// while adb still cannot reach it, restart the stale adb server and connect once more.
    async fn recover(&self) -> Reach {
        self.ready.store(false, Ordering::SeqCst);
        let serial = self.serial();
        self.adb(&["disconnect", &serial], CONNECT_SECS).await;
        self.rediscover().await;
        let r = self.connect().await;
        if r.ok || !matches!(r.why, "unreachable" | "offline") {
            return r;
        }
        self.restart_server(r).await
    }

    async fn restart_server(&self, failed: Reach) -> Reach {
        let now = (self.clock)();
        if let Some(at) = *self.restarted_at.lock() {
            if now - at < ADB_RESTART_EVERY {
                return failed;
            }
        }
        let (host, port) = {
            let cfg = self.cfg.lock();
            (host_of(&cfg), port_of(&cfg))
        };
        if !self.probe.probe(host, port, PROBE_SECS).await {
            return failed; // the TV itself is not answering: the server is not the problem
        }
        *self.restarted_at.lock() = Some(now);
        tracing::warn!("firetv: the TV's port answers but adb cannot reach it; restarting the adb server");
        self.adb(&["kill-server"], CONNECT_SECS).await;
        self.adb(&["start-server"], CONNECT_SECS).await;
        let r = self.connect().await;
        tracing::info!("firetv: after the adb server restart: {}", if r.ok { "connected" } else { r.why });
        r
    }

    pub async fn ensure(&self) -> Reach {
        let _held = self.lock.lock().await;
        if self.ready.load(Ordering::SeqCst) {
            return Reach::ok();
        }
        let r = self.connect().await;
        if r.ok || matches!(r.why, "no_adb" | "unauthorized") {
            return r;
        }
        self.recover().await
    }

    /// One `adb shell` on the TV, reconnecting and retrying once if the TV went away under it. `again = false` is
    /// for a command that must not land twice (typing, a run of keys, a toggle): after a timeout it may already
    /// have landed, so it is not sent again; after "device not found" it never ran, so it is.
    pub async fn shell(&self, script: &str, timeout: f64, again: bool) -> (Reach, i32, String) {
        let r = self.ensure().await;
        if !r.ok {
            return (r, 0, String::new());
        }
        let (rc, out) = self.adb(&["-s", &self.serial(), "shell", script], timeout).await;
        if !lost(rc, &out) {
            return (r, rc, out);
        }
        if !again && !gone(rc, &out) {
            self.ready.store(false, Ordering::SeqCst); // connect afresh next time, but don't press it twice
            return (Reach::fail("timeout", TIMEOUT_LINE), rc, out);
        }
        let r = {
            let _held = self.lock.lock().await;
            self.recover().await
        };
        if !r.ok {
            return (r, 0, String::new());
        }
        let (rc, out) = self.adb(&["-s", &self.serial(), "shell", script], timeout).await;
        if lost(rc, &out) {
            self.ready.store(false, Ordering::SeqCst);
            return (Reach::fail("offline", UNREACHABLE_LINE), rc, out);
        }
        (r, rc, out)
    }

    // -- reading --
    pub async fn state(&self) -> (Reach, Option<TvState>) {
        let (r, _, out) = self.shell(STATE_SCRIPT, SHELL_SECS, true).await;
        if !r.ok {
            return (r, None);
        }
        if !out.contains("@@POWER") {
            self.ready.store(false, Ordering::SeqCst); // whatever answered, connect afresh next time
            return (Reach::fail("offline", "The TV answered, but not with its state."), None);
        }
        (r, Some(parse_state(&out)))
    }

    pub async fn status(&self) -> Out {
        let t0 = (self.clock)();
        let (r, st) = self.state().await;
        self.log("status", r.ok, t0);
        match st {
            None => obj(vec![
                ("ok", Value::Bool(false)),
                ("reachable", Value::Bool(!matches!(r.why, "unreachable" | "offline" | "no_adb"))),
                ("authorized", Value::Bool(r.why != "unauthorized")),
                ("reason", Value::String(r.line.into())),
                ("line", Value::String(r.line.into())),
            ]),
            Some(st) => {
                let mut out = obj(vec![("ok", Value::Bool(true))]);
                out.extend(st.public());
                out.insert("line".into(), Value::String(said_state(&st)));
                out
            }
        }
    }

    // -- doing --
    /// One command; `{"ok", "line", ...}`. The line is what to tell the owner.
    pub async fn control(&self, action: &str, args: ControlArgs) -> Out {
        let t0 = (self.clock)();
        let args = ControlArgs { app: py_strip(&args.app).to_string(), query: py_strip(&args.query).to_string(), ..args };
        let out = self.control_inner(action, args).await;
        self.log(action, is_ok(&out), t0);
        out
    }

    fn log(&self, action: &str, ok: bool, t0: f64) {
        // the action, the outcome and the time: never typed text, a search or an app's words
        tracing::info!(
            "firetv: {} -> {} in {:.0} ms",
            action,
            if ok { "done" } else { "failed" },
            ((self.clock)() - t0) * 1000.0
        );
    }

    fn fail_with(line: &str, extra: Out) -> Out {
        let base = obj(vec![
            ("ok", Value::Bool(false)),
            ("reason", Value::String(line.into())),
            ("line", Value::String(line.into())),
        ]);
        merged(base, extra)
    }

    fn fail(line: &str) -> Out {
        Self::fail_with(line, Out::new())
    }

    /// Run a command; (a failure or None, rc, output). `again`: as in [`FireTV::shell`].
    async fn send(&self, script: &str, timeout: f64, again: bool) -> (Option<Out>, i32, String) {
        let (r, rc, out) = self.shell(script, timeout, again).await;
        if !r.ok {
            let extra = obj(vec![
                ("reachable", Value::Bool(!matches!(r.why, "unreachable" | "offline" | "no_adb"))),
                ("authorized", Value::Bool(r.why != "unauthorized")),
            ]);
            return (Some(Self::fail_with(r.line, extra)), rc, out);
        }
        (None, rc, out)
    }

    async fn control_inner(&self, action: &str, args: ControlArgs) -> Out {
        let action = key_alias(action);
        if action == "status" {
            return self.status().await;
        }
        if action == "power_on" {
            return self.power(true).await;
        }
        if action == "power_off" {
            return self.power(false).await;
        }
        if nav_key(action).is_some() {
            return self.press(action, args.repeat.unwrap_or(1)).await;
        }
        if let Some(dispatch) = media_key(action) {
            // play-pause, next and the seeks move on each press: not sent twice after a timeout
            let script = sh(&["cmd", "media_session", "dispatch", dispatch]);
            let (fail, rc, out) = self.send(&script, SHELL_SECS, matches!(action, "play" | "pause" | "stop")).await;
            if let Some(fail) = fail {
                return fail;
            }
            if rc != 0 {
                return Self::fail(&format!("The TV refused {} ({}).", action.replace('_', " "), clean(&out, 80)));
            }
            let said = match action {
                "play_pause" => "Pressed play/pause".to_string(),
                "fast_forward" => "Pressed fast-forward".to_string(),
                _ => format!("Pressed {action}"),
            };
            return ok_line(said + " on the TV.");
        }
        if action == "volume" {
            return self.volume(args.volume, args.volume_change).await;
        }
        if action == "mute" || action == "unmute" {
            return self.mute(action == "mute").await;
        }
        if action == "open_app" {
            return self.open_app(&args.app).await;
        }
        if action == "youtube_search" {
            return self.youtube_search(&args.query).await;
        }
        if action == "type_text" {
            return self.type_text(&args.text).await;
        }
        Self::fail(&format!("I can't do {} on the TV.", py_repr_str(&clean(action, 40))))
    }

    async fn power(&self, on: bool) -> Out {
        let (r, st) = self.state().await;
        let Some(st) = st else {
            let reach_lost = matches!(r.why, "unreachable" | "offline");
            if on && reach_lost {
                return Self::fail_with(DEEP_STANDBY_LINE, obj(vec![("reachable", Value::Bool(false))]));
            }
            if !on && reach_lost {
                return Self::fail_with(
                    "I can't reach the TV. It may already be off.",
                    obj(vec![("reachable", Value::Bool(false))]),
                );
            }
            return Self::fail_with(
                r.line,
                obj(vec![
                    ("reachable", Value::Bool(r.why != "no_adb")),
                    ("authorized", Value::Bool(r.why != "unauthorized")),
                ]),
            );
        };
        if on && st.power == "on" {
            return merged(ok_line("The TV is already on."), st.public());
        }
        if !on && st.power == "standby" {
            return merged(ok_line("The TV is already off."), st.public());
        }
        // WAKEUP and SLEEP do nothing when the TV is already there; POWER is a toggle and is never sent
        let code = if on { KEYCODE_WAKEUP } else { KEYCODE_SLEEP }.to_string();
        let (fail, _, _) = self.send(&sh(&["input", "keyevent", &code]), INPUT_SECS, true).await;
        if let Some(fail) = fail {
            return fail;
        }
        let mut waited = 0.0;
        let mut last = st;
        loop {
            let (_, now) = self.state().await;
            if let Some(now) = now {
                last = now.clone();
                if (on && now.power == "on") || (!on && now.power == "standby") {
                    let line = if on {
                        "TV on".to_string() + &if now.app.is_empty() { ".".into() } else { format!(" — {}.", now.app) }
                    } else {
                        "TV off.".to_string()
                    };
                    return merged(ok_line(line), now.public());
                }
            }
            if waited >= POWER_POLL_SECS {
                break;
            }
            (self.sleep)(POLL_STEP_SECS).await;
            waited += POLL_STEP_SECS;
        }
        let word = if on { "wake" } else { "sleep" };
        Self::fail_with(&format!("I sent {word}, but the TV still reports {}.", last.power), last.public())
    }

    /// A d-pad key, pressed `repeat` times in ONE `input` call (each call costs a Java VM start).
    pub async fn press(&self, key: &str, repeat: i64) -> Out {
        let key = key_alias(key);
        let Some(code) = nav_key(key) else {
            return Self::fail(&format!("The TV has no {} key here.", py_repr_str(&clean(key, 20))));
        };
        let n = (if repeat == 0 { 1 } else { repeat }).clamp(1, MAX_REPEAT);
        let code = code.to_string();
        let mut words = vec!["input", "keyevent"];
        words.extend(std::iter::repeat_n(code.as_str(), n as usize));
        let (fail, rc, out) = self.send(&sh(&words), INPUT_SECS + 0.3 * n as f64, false).await;
        if let Some(fail) = fail {
            return fail;
        }
        if rc != 0 {
            return Self::fail(&format!("The TV refused the key ({}).", clean(&out, 80)));
        }
        let line = format!("Pressed {key}") + &if n > 1 { format!(" {n} times.") } else { ".".into() };
        let mut out = ok_line(line);
        out.insert("pressed".into(), Value::from(n));
        out
    }

    async fn volume(&self, volume: Option<i64>, change: Option<i64>) -> Out {
        if volume.is_none() && change.is_none() {
            return Self::fail("What volume?");
        }
        let (r, st) = self.state().await;
        let Some(st) = st else {
            return Self::fail(r.line);
        };
        let (Some(current), Some(max_index)) = (st.volume(), st.max_index) else {
            return Self::fail("I couldn't read the TV's volume, so I didn't change it.");
        };
        let target = match volume {
            Some(v) => v,
            None => current.saturating_add(change.unwrap_or(0)),
        }
        .clamp(0, 100);
        let index = py_round(target as f64 * max_index as f64 / 100.0).to_string();
        let (fail, rc, out) =
            self.send(&sh(&["cmd", "media_session", "volume", "--stream", "3", "--set", &index]), SHELL_SECS, true).await;
        if let Some(fail) = fail {
            return fail;
        }
        if rc != 0 {
            return Self::fail(&format!("The TV refused the volume ({}).", clean(&out, 80)));
        }
        let (_, after) = self.state().await;
        match after.as_ref().and_then(|a| a.volume().map(|v| (a, v))) {
            None => {
                let mut out = ok_line(format!("Set the TV volume to {target}%, but couldn't read it back."));
                out.insert("volume".into(), Value::Null);
                out
            }
            Some((after, v)) => {
                let muted = after.muted == Some(true);
                let mut out = ok_line(format!("TV volume {v}%") + if muted { " (muted)." } else { "." });
                out.insert("volume".into(), Value::from(v));
                out.insert("muted".into(), after.muted.map_or(Value::Null, Value::Bool));
                out
            }
        }
    }

    async fn mute(&self, mute: bool) -> Out {
        let (r, st) = self.state().await;
        let Some(st) = st else {
            return Self::fail(r.line);
        };
        let Some(muted) = st.muted else {
            return Self::fail("I couldn't read whether the TV is muted, so I didn't touch it.");
        };
        if muted == mute {
            let line = "The TV is already ".to_string() + if mute { "muted." } else { "unmuted." };
            return merged(ok_line(line), st.public());
        }
        // KEYCODE_VOLUME_MUTE toggles the speaker; media_session's "mute" is the microphone's key
        // ok only when the key was taken AND the readback shows the target: a toggle can be refused, or answered
        // and still lost, so an unread one is not ok (verification/Eve/FireTv.lean, Fix.step, 2026-10-04)
        let (fail, rc, out) =
            self.send(&sh(&["input", "keyevent", &KEYCODE_VOLUME_MUTE.to_string()]), INPUT_SECS, false).await;
        if let Some(fail) = fail {
            return fail;
        }
        if rc != 0 {
            return Self::fail(&format!("The TV refused the mute key ({}).", clean(&out, 80)));
        }
        let (_, after) = self.state().await;
        let Some(after) = after.filter(|a| a.muted.is_some()) else {
            return Self::fail(
                &("I pressed mute, but couldn't read the TV back, so I can't say it is ".to_string()
                    + if mute { "muted." } else { "unmuted." }),
            );
        };
        if after.muted != Some(mute) {
            return Self::fail_with(
                &("I pressed mute, but the TV still reports ".to_string() + if mute { "unmuted." } else { "muted." }),
                after.public(),
            );
        }
        let line = if mute {
            "TV muted.".to_string()
        } else {
            format!("TV unmuted, volume {}%.", after.volume().map_or("None".to_string(), |v| v.to_string()))
        };
        merged(ok_line(line), after.public())
    }

    /// What is in front now, and the TV woken first when it is in standby (an app opened on a dark screen is no
    /// use).
    async fn before_opening(&self) -> (Option<TvState>, &'static str) {
        let (_, st) = self.state().await;
        let Some(st) = st else {
            return (None, "");
        };
        let mut woke = "";
        if st.power == "standby" {
            let (fail, _, _) = self.send(&sh(&["input", "keyevent", &KEYCODE_WAKEUP.to_string()]), INPUT_SECS, true).await;
            if fail.is_none() {
                woke = "Turned the TV on. ";
            }
        }
        (Some(st), woke)
    }

    fn interrupted(st: Option<&TvState>, target: &str) -> String {
        match st {
            Some(st)
                if st.power != "standby" && !st.app.is_empty() && !["home", "screensaver", target].contains(&st.app.as_str()) =>
            {
                st.app.clone()
            }
            _ => String::new(),
        }
    }

    async fn start(&self, component: &str, data: &str) -> (Option<Out>, bool) {
        let mut words = vec!["am", "start"];
        if !data.is_empty() {
            words.extend(["-a", "android.intent.action.VIEW", "-d", data]);
        }
        words.extend(["-n", component]);
        let (fail, rc, out) = self.send(&sh(&words), SHELL_SECS, true).await;
        if fail.is_some() {
            return (fail, false);
        }
        (None, rc == 0 && !am_failed(&out))
    }

    /// The package's launcher activity: LEANBACK_LAUNCHER first (the TV's own), then LAUNCHER.
    async fn resolve(&self, package: &str) -> String {
        if !PACKAGE_RE.is_match(package) {
            return String::new();
        }
        let lean_cat = "android.intent.category.LEANBACK_LAUNCHER";
        let plain_cat = "android.intent.category.LAUNCHER";
        let script = sh(&["cmd", "package", "resolve-activity", "--brief", "-c", lean_cat, package])
            + "; echo @@; "
            + &sh(&["cmd", "package", "resolve-activity", "--brief", "-c", plain_cat, package]);
        let (r, _, out) = self.shell(&script, SHELL_SECS, true).await;
        if !r.ok {
            return String::new();
        }
        let (lean, plain) = out.split_once("@@").unwrap_or((out.as_str(), ""));
        let found = resolved_component(lean);
        if found.is_empty() {
            resolved_component(plain)
        } else {
            found
        }
    }

    pub async fn packages(&self) -> Vec<String> {
        let (r, _, out) = self.shell("pm list packages", SHELL_SECS, true).await;
        if !r.ok {
            return Vec::new();
        }
        let set: BTreeSet<String> =
            PM_PACKAGE_RE.captures_iter(&out).filter_map(|c| c.get(1).map(|m| m.as_str().to_string())).collect();
        set.into_iter().collect()
    }

    /// By the table's name or alias; else any installed package whose name has the words in it.
    pub async fn open_app(&self, name: &str) -> Out {
        let key = app_key(name);
        if key.is_empty() {
            return Self::fail("Which app?");
        }
        let (st, woke) = self.before_opening().await;
        let entry = find_app(name);
        let mut label = entry.map_or_else(|| clean(name, 40), |e| e.name.to_string());
        let (component, data) = entry.map_or(("", ""), |e| (e.component, e.data));
        let mut started = false;
        if !component.is_empty() {
            let (fail, ok) = self.start(component, data).await;
            if let Some(fail) = fail {
                return fail;
            }
            started = ok;
        }
        if !started {
            let mut package = entry.map_or(String::new(), |e| e.package().to_string());
            if package.is_empty() {
                let squashed = key.replace(' ', "");
                let found: Vec<String> = self
                    .packages()
                    .await
                    .into_iter()
                    .filter(|p| p.to_lowercase().replace('_', "").contains(&squashed))
                    .collect();
                if found.is_empty() {
                    return Self::fail(&format!("There's no app called {label} on the TV."));
                }
                if found.len() > 1 {
                    let names = found.iter().take(5).map(|p| clean(p, 60)).collect::<Vec<_>>().join(", ");
                    return Self::fail(&format!("More than one app on the TV matches {label}: {names}."));
                }
                package = found[0].clone();
            }
            let resolved = self.resolve(&package).await;
            if resolved.is_empty() {
                return Self::fail(&format!("{label} is on the TV but has nothing I can open."));
            }
            let (fail, ok) = self.start(&resolved, "").await;
            if let Some(fail) = fail {
                return fail;
            }
            if !ok {
                return Self::fail(&format!("The TV wouldn't open {label}."));
            }
            if entry.is_none() {
                label = app_name(&package, "");
            }
        }
        let was = Self::interrupted(st.as_ref(), &label);
        let line = format!("{woke}Opened {label}") + &if was.is_empty() { ".".into() } else { format!(" — {was} was on.") };
        let mut out = ok_line(line);
        out.insert("opened".into(), Value::String(label));
        if !was.is_empty() {
            out.insert("interrupted".into(), Value::String(was));
        }
        out
    }

    pub async fn youtube_search(&self, query: &str) -> Out {
        let q = clean(query, MAX_TEXT);
        if q.is_empty() {
            return Self::fail("Search YouTube for what?");
        }
        let (st, woke) = self.before_opening().await;
        let url = "https://www.youtube.com/results?search_query=".to_string() + &quote_plus(&q);
        // three apps take a youtube.com link on this TV (YouTube, Silk, Amazon's intent helper): -n pins YouTube
        let (fail, started) = self.start(YOUTUBE.component, &url).await;
        if let Some(fail) = fail {
            return fail;
        }
        if !started {
            return Self::fail("The TV wouldn't open YouTube's search.");
        }
        let was = Self::interrupted(st.as_ref(), YOUTUBE.name);
        let line = format!("{woke}Searching YouTube on the TV.") + &if was.is_empty() { String::new() } else { format!(" {was} was on.") };
        let mut out = ok_line(line);
        if !was.is_empty() {
            out.insert("interrupted".into(), Value::String(was));
        }
        out
    }

    pub async fn type_text(&self, text: &str) -> Out {
        let (arg, why) = type_text_arg(text);
        if !why.is_empty() {
            return Self::fail(&why);
        }
        let timeout = INPUT_SECS + arg.chars().count() as f64 * 0.02;
        let (fail, rc, out) = self.send(&sh(&["input", "text", &arg]), timeout, false).await;
        if let Some(fail) = fail {
            return fail;
        }
        if rc != 0 {
            return Self::fail(&format!("The TV refused the text ({}).", clean(&out, 80)));
        }
        ok_line(format!("Typed {} characters on the TV.", text.chars().count()))
    }

    /// A PNG of the screen, saved under `~/.cache/eve-bridge/firetv/`: `{"ok", "path"}`. An error is a file that
    /// could not be written (Python raises there).
    pub async fn screenshot(&self, out_dir: Option<&Path>, out_path: Option<&Path>) -> io::Result<Out> {
        let t0 = (self.clock)();
        let r = self.ensure().await;
        if !r.ok {
            self.log("screenshot", false, t0);
            return Ok(Self::fail(r.line));
        }
        let mut argv: Vec<String> =
            [self.adb.as_str(), "-s", &self.serial(), "exec-out", "screencap", "-p"].iter().map(|s| s.to_string()).collect();
        let (mut rc, mut data) = self.capture.capture(argv.clone(), SCREENCAP_SECS).await;
        if rc != 0 && lost(rc, &String::from_utf8_lossy(&data[..data.len().min(300)])) {
            let r = {
                let _held = self.lock.lock().await;
                self.recover().await
            };
            if !r.ok {
                self.log("screenshot", false, t0);
                return Ok(Self::fail(r.line));
            }
            argv[2] = self.serial();
            (rc, data) = self.capture.capture(argv, SCREENCAP_SECS).await;
        }
        if rc != 0 || !data.starts_with(PNG_SIGNATURE) {
            self.log("screenshot", false, t0);
            return Ok(Self::fail("The TV didn't send a picture."));
        }
        let target = match out_path {
            Some(p) => expanduser(p),
            None => {
                let folder = out_dir
                    .map(Path::to_path_buf)
                    .or_else(|| self.shots_dir.clone())
                    .unwrap_or_else(|| PathBuf::from(SHOTS_DIR));
                expanduser(&folder).join(chrono::Local::now().format("firetv-%Y%m%d-%H%M%S.png").to_string())
            }
        };
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&target, &data)?;
        self.log("screenshot", true, t0);
        Ok(obj(vec![("ok", Value::Bool(true)), ("path", Value::String(target.to_string_lossy().into_owned()))]))
    }

    // -- the model's tools --
    pub fn tools(&self) -> Vec<Value> {
        TOOLS.clone()
    }

    /// `screenshot = false` where the screenshot tool is not lent (the live voice): no sentence names it.
    pub fn instructions(&self, screenshot: bool) -> &'static str {
        if screenshot {
            INSTRUCTIONS.as_str()
        } else {
            INSTRUCTIONS_NO_SCREENSHOT.as_str()
        }
    }

    /// The screenshot for a model that cannot see it: the picture's path for the owner, and in words what the TV
    /// reports at the same moment (`shows`), so the model is told something true about the screen.
    pub async fn screenshot_for_model(&self) -> io::Result<Out> {
        let shot = self.screenshot(None, None).await?;
        let path = get_str(&shot, "path");
        if !is_ok(&shot) || path.is_empty() {
            let reason = get_str(&shot, "reason");
            let reason = if reason.is_empty() { "the TV sent no picture".to_string() } else { reason };
            return Ok(obj(vec![("ok", Value::Bool(false)), ("reason", Value::String(reason))]));
        }
        let (_, st) = self.state().await;
        let shows = st.as_ref().map_or("the TV's state couldn't be read".to_string(), said_state);
        Ok(obj(vec![
            ("ok", Value::Bool(true)),
            ("path", Value::String(path)),
            ("shows", Value::String(shows)),
        ]))
    }

    /// The model's three tools. An error only from the screenshot's file write.
    pub async fn handle(&self, name: &str, args: &Value) -> io::Result<Out> {
        if name == "firetv_status" {
            return Ok(self.status().await);
        }
        if name == "firetv_screenshot" {
            return self.screenshot_for_model().await;
        }
        if name == "firetv_control" {
            let a = ControlArgs {
                app: arg_str(args, "app", ""),
                query: arg_str(args, "query", ""),
                text: arg_str(args, "text", ""),
                volume: num(args.get("volume")),
                volume_change: num(args.get("volume_change")),
                repeat: num(args.get("repeat")),
            };
            return Ok(self.control(&arg_str(args, "action", ""), a).await);
        }
        Ok(obj(vec![("ok", Value::Bool(false)), ("reason", Value::String(format!("unknown tool {name}")))]))
    }

    // -- the Mini App's remote --
    /// The Mini App's TV remote (miniapp.py `/api/firetv`, owner only). `{"action": "status"}` reads the TV; every
    /// other action in [`PANEL_ACTIONS`] goes through `control` and answers with the fresh state, so the card
    /// always shows what the TV reports, not what was asked. `volume` takes `volume` (0-100, the slider),
    /// `volume_change` takes `change` (a signed percent), `open_app` takes `app` (a name from [`APPS`]), a d-pad
    /// key takes an optional `repeat`. The answer always carries `apps` (the picker's names) and `now` (the status
    /// line) when the TV could be read; `line` says what happened.
    pub async fn panel(&self, body: &Value) -> Out {
        let apps = || Value::Array(APPS.iter().map(|a| Value::String(a.name.into())).collect());
        let refuse = |line: &str| {
            obj(vec![("ok", Value::Bool(false)), ("line", Value::String(line.into())), ("apps", apps())])
        };
        let action = arg_str(body, "action", "status");
        if action == "status" {
            let st = self.status().await;
            let now = if is_ok(&st) { st.get("line").cloned().unwrap_or(Value::Null) } else { Value::Null };
            let mut out = st;
            out.insert("now".into(), now);
            out.insert("apps".into(), apps());
            return out;
        }
        if !PANEL_ACTIONS.contains(&action.as_str()) {
            return refuse("Not something the TV remote does.");
        }
        let out = if action == "volume" {
            let Some(volume) = num(body.get("volume")) else {
                return refuse("What volume?");
            };
            self.control("volume", ControlArgs { volume: Some(volume.clamp(0, 100)), ..Default::default() }).await
        } else if action == "volume_change" {
            let change = num(body.get("change")).unwrap_or(0);
            if change == 0 {
                return refuse("Louder or quieter?");
            }
            self.control("volume", ControlArgs { volume_change: Some(change.clamp(-100, 100)), ..Default::default() })
                .await
        } else if action == "open_app" {
            let Some(entry) = find_app(&arg_str(body, "app", "")) else {
                return refuse("That app isn't on the remote.");
            };
            self.control("open_app", ControlArgs { app: entry.name.into(), ..Default::default() }).await
        } else if nav_key(&action).is_some() {
            // taps made while one was on its way arrive as one press with a count: one `input` call, not several
            self.control(&action, ControlArgs { repeat: num(body.get("repeat")), ..Default::default() }).await
        } else {
            self.control(&action, ControlArgs::default()).await
        };
        let line = {
            let l = get_str(&out, "line");
            if l.is_empty() {
                get_str(&out, "reason")
            } else {
                l
            }
        };
        if !is_ok(&out) && out.get("reachable") == Some(&Value::Bool(false)) {
            return refuse(&line); // the TV is gone: don't wait on it twice
        }
        let (_, st) = self.state().await;
        let mut fresh = match st {
            Some(st) => {
                let mut f = st.public();
                f.insert("now".into(), Value::String(said_state(&st)));
                f
            }
            None => Out::new(),
        };
        fresh.insert("ok".into(), Value::Bool(is_ok(&out)));
        fresh.insert("line".into(), Value::String(line));
        fresh.insert("apps".into(), apps());
        fresh
    }

    // -- a message that is only a TV command --
    pub fn match_command(&self, text: &str) -> Option<Command> {
        match_command(text)
    }

    pub async fn run(&self, cmd: &Command) -> String {
        let args = ControlArgs {
            app: cmd.app.clone(),
            query: cmd.query.clone(),
            text: cmd.text.clone(),
            volume: cmd.volume,
            volume_change: cmd.change,
            repeat: Some(cmd.repeat),
        };
        let out = self.control(&cmd.action, args).await;
        tracing::info!("firetv: by code, {} -> {}", cmd.action, if is_ok(&out) { "done" } else { "refused" });
        let line = get_str(&out, "line");
        if line.is_empty() {
            get_str(&out, "reason")
        } else {
            line
        }
    }

    pub async fn close(&self) {}
}

/// Rows of `adb mdns services`: `adb-<serial>  _adb._tcp  host:port`. Only plain ADB, not the TLS one.
pub fn parse_mdns(text: &str) -> Vec<(String, String, i64)> {
    let mut rows = Vec::new();
    for line in pyfmt::splitlines(text) {
        let parts: Vec<&str> = line.split(is_py_space).filter(|p| !p.is_empty()).collect();
        if parts.len() >= 3 && parts[1].trim_end_matches('.') == "_adb._tcp" {
            let (host, port) = parts[2].rsplit_once(':').unwrap_or(("", parts[2]));
            if let (false, Some(port)) = (host.is_empty(), pyfmt::parse_decimal(port)) {
                rows.push((clean(parts[0], 80), host.to_string(), port));
            }
        }
    }
    rows
}

// ---- construction --------------------------------------------------------------------------------------------

pub fn enabled(environ: Option<&Environ>) -> bool {
    let v = env_get(environ, "EVE_FIRETV");
    !matches!(py_strip(&v).to_lowercase().as_str(), "0" | "false" | "no" | "off")
}

/// The owner's Fire TV, or None when switched off or never paired (then no tool is lent).
pub fn make_firetv(
    path: Option<&Path>,
    environ: Option<&Environ>,
    runner: Option<Arc<dyn Runner>>,
    capture: Option<Arc<dyn Capture>>,
) -> Option<FireTV> {
    if !enabled(environ) {
        tracing::info!("firetv: off (EVE_FIRETV)");
        return None;
    }
    let p = cfg_path(path);
    let cfg = load(Some(&p));
    if cfg.is_empty() {
        tracing::info!("firetv: not paired (eve-bridge firetv pair)");
        return None;
    }
    tracing::info!("firetv: on");
    let mut tv = FireTV::new(cfg).with_adb(&find_adb(environ)).with_path(Some(p));
    if let Some(r) = runner {
        tv = tv.with_runner(r);
    }
    if let Some(c) = capture {
        tv = tv.with_capture(c);
    }
    Some(tv)
}

// ---- the CLI -------------------------------------------------------------------------------------------------

/// Where the CLI's lines go (Python's `out=print`).
pub type OutFn<'a> = &'a mut (dyn FnMut(&str) + Send);

/// Find the TV (by mDNS unless a host is given), connect, check it allows this Mac, and save it.
pub async fn pair(
    host: &str,
    port: i64,
    path: Option<&Path>,
    out: OutFn<'_>,
    runner: Option<Arc<dyn Runner>>,
    environ: Option<&Environ>,
) -> i32 {
    let adb = find_adb(environ);
    if adb.is_empty() {
        out(INSTALL_LINE);
        return 1;
    }
    let make = |host: &str, port: i64, mdns: Option<&str>| {
        let mut cfg = obj(vec![("host", Value::String(host.into())), ("port", Value::from(port))]);
        if let Some(m) = mdns {
            cfg.insert("mdns".into(), Value::String(m.into()));
        }
        let tv = FireTV::new(cfg).with_adb(&adb);
        match &runner {
            Some(r) => tv.with_runner(r.clone()),
            None => tv,
        }
    };
    let probe = make(if host.is_empty() { "0.0.0.0" } else { host }, port, None);
    let found = probe.discover().await;
    let (mut mdns, host, port) = if host.is_empty() {
        if found.is_empty() {
            out("No Fire TV found on the network. On the TV: Settings > My Fire TV (Device & Software on some \
                 TVs) > Developer Options > ADB Debugging on. Or give its address: eve-bridge firetv pair --host ADDRESS");
            return 1;
        }
        if found.len() > 1 {
            out("More than one ADB device is on the network. Pick one with --host:");
            for (name, h, pt) in &found {
                out(&format!("  {h}:{pt}  {name}"));
            }
            return 1;
        }
        found[0].clone()
    } else {
        let mdns = found.iter().find(|(_, h, pt)| h == host && *pt == port).map(|(n, _, _)| n.clone());
        (mdns.unwrap_or_default(), host.to_string(), port)
    };
    let tv = make(&host, port, Some(&mdns));
    let r = tv.connect().await;
    if !r.ok {
        if r.why != "unauthorized" {
            out(r.line);
        } else {
            out(&format!("{ALLOW_LINE} Then run this again."));
        }
        return 1;
    }
    let script = sh(&["getprop", "ro.product.model"]) + "; echo @@; " + &sh(&["getprop", "ro.serialno"]);
    let (_, _, text) = tv.shell(&script, SHELL_SECS, true).await;
    let (model_raw, serial_raw) = text.split_once("@@").unwrap_or((text.as_str(), ""));
    let (model, serial) = (clean(model_raw, 60), clean(serial_raw, 60));
    if mdns.is_empty() && SERIAL_RE.is_match(&serial) {
        mdns = format!("adb-{serial}"); // the name adbd advertises itself by: kept to find the TV again
    }
    let cfg = obj(vec![
        ("host", Value::String(host.clone())),
        ("port", Value::from(port)),
        ("mdns", Value::String(mdns)),
        ("model", Value::String(model.clone())),
    ]);
    let p = match save(&cfg, path) {
        Ok(p) => p,
        Err(e) => {
            out(&format!("Could not save the TV to {} ({e}).", cfg_path(path).display()));
            return 1;
        }
    };
    let who = if model.is_empty() { "the TV" } else { model.as_str() };
    out(&format!(
        "Paired with {who} at {host}:{port}. Saved to {} (private). Restart the daemon to lend it to the chat and \
         the voice.",
        p.display()
    ));
    0
}

fn cli_parser() -> clap::Command {
    use clap::{value_parser, Arg, ArgAction, Command as C};
    let keys: Vec<&str> = NAV_KEYS.iter().map(|(k, _)| *k).chain(MEDIA_KEYS.iter().map(|(k, _)| *k)).collect();
    let rest = |name: &'static str| Arg::new(name).num_args(1..).required(true).action(ArgAction::Append);
    C::new("eve-bridge firetv")
        .subcommand_required(true)
        .infer_long_args(true)
        .subcommand(
            C::new("pair")
                .about("Find the Fire TV on the network and save it (ADB debugging on, on the TV)")
                .arg(Arg::new("host").long("host").default_value("").help("the TV's address, when mDNS doesn't find it"))
                .arg(Arg::new("port").long("port").value_parser(value_parser!(i64)).default_value("5555")),
        )
        .subcommand(C::new("forget").about("Forget the paired TV"))
        .subcommand(C::new("status").about("Power, the app in front, the volume"))
        .subcommand(C::new("on").about("Wake the TV"))
        .subcommand(C::new("off").about("Put the TV to sleep"))
        .subcommand(
            C::new("key")
                .about(format!("Press a remote key: {}", keys.join(", ")))
                .arg(Arg::new("key").required(true))
                .arg(Arg::new("times").long("times").value_parser(value_parser!(i64)).default_value("1")),
        )
        .subcommand(
            C::new("volume").about("Set the volume (0-100) or step it (up, down)").arg(Arg::new("level").required(true)),
        )
        .subcommand(C::new("mute"))
        .subcommand(C::new("unmute"))
        .subcommand(C::new("open").about("Open an app by name").arg(rest("app")))
        .subcommand(C::new("apps").about("The apps eve knows by name that are on the TV"))
        .subcommand(C::new("youtube").about("Search YouTube on the TV").arg(rest("query")))
        .subcommand(C::new("type").about("Type into the TV's focused text box").arg(rest("text")))
        .subcommand(C::new("screenshot").about("Save a PNG of the screen").arg(Arg::new("out").long("out").default_value("")))
}

/// What [`cli`] takes besides argv (Python's keyword arguments).
#[derive(Default)]
pub struct CliOptions {
    pub path: Option<PathBuf>,
    pub runner: Option<Arc<dyn Runner>>,
    pub capture: Option<Arc<dyn Capture>>,
    pub environ: Option<Environ>,
}

/// `eve-bridge firetv pair [--host H] [--port P] | status | on | off | key KEY [--times N] | volume N|up|down
/// | mute | unmute | open APP | apps | youtube QUERY | type TEXT | screenshot [--out PATH] | forget`.
/// Runs on its own runtime, as Python's `asyncio.run` does; [`cli_async`] is the same inside one.
pub fn cli(argv: &[String], out: OutFn<'_>, opts: CliOptions) -> i32 {
    match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt.block_on(cli_async(argv, out, opts)),
        Err(e) => {
            out(&format!("Could not start the runtime ({e})."));
            1
        }
    }
}

pub async fn cli_async(argv: &[String], out: OutFn<'_>, opts: CliOptions) -> i32 {
    let matches = match cli_parser().try_get_matches_from(std::iter::once("eve-bridge firetv".to_string()).chain(argv.iter().cloned()))
    {
        Ok(m) => m,
        Err(e) => {
            let _ = e.print();
            return e.exit_code();
        }
    };
    let (act, a) = matches.subcommand().expect("a subcommand is required");
    let s = |name: &str| a.get_one::<String>(name).cloned().unwrap_or_default();
    let words = |name: &str| a.get_many::<String>(name).map(|v| v.cloned().collect::<Vec<_>>().join(" ")).unwrap_or_default();
    let cfg_file = cfg_path(opts.path.as_deref());
    let environ = opts.environ.as_ref();

    if act == "pair" {
        let port = a.get_one::<i64>("port").copied().unwrap_or(DEFAULT_PORT);
        return pair(py_strip(&s("host")), port, Some(&cfg_file), out, opts.runner, environ).await;
    }
    if act == "forget" {
        return match std::fs::remove_file(&cfg_file) {
            Ok(()) => {
                out(&format!("Forgot the TV ({}).", cfg_file.display()));
                0
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                out("No TV was paired.");
                0
            }
            Err(e) => {
                out(&format!("Could not forget the TV ({}): {e}", cfg_file.display()));
                1
            }
        };
    }
    let cfg = load(Some(&cfg_file));
    if cfg.is_empty() {
        out(&format!("No TV paired ({}). Run: eve-bridge firetv pair", cfg_file.display()));
        return 1;
    }
    let model = match cfg.get("model") {
        Some(v) if py_truthy(v) => Some(py_str(v)),
        _ => None,
    };
    let mut tv = FireTV::new(cfg).with_adb(&find_adb(environ)).with_path(Some(cfg_file.clone()));
    if let Some(r) = opts.runner {
        tv = tv.with_runner(r);
    }
    if let Some(c) = opts.capture {
        tv = tv.with_capture(c);
    }
    let control = |action: &'static str, args: ControlArgs| {
        let tv = &tv;
        async move { tv.control(action, args).await }
    };
    let failed = |line: &str| obj(vec![("ok", Value::Bool(false)), ("line", Value::String(line.into()))]);

    let result: Out = match act {
        "status" => {
            let mut st = tv.status().await;
            if is_ok(&st) {
                if let Some(model) = &model {
                    let line = format!("{}\n{} at {}", get_str(&st, "line"), clean(model, 60), tv.serial());
                    st.insert("line".into(), Value::String(line));
                }
            }
            st
        }
        "on" => control("power_on", ControlArgs::default()).await,
        "off" => control("power_off", ControlArgs::default()).await,
        "key" => {
            let raw = s("key");
            let lower = raw.to_lowercase();
            let key = match KEY_ALIASES.iter().find(|(k, _)| *k == lower) {
                Some((_, v)) => v.to_string(),
                None => lower.replace('-', "_"),
            };
            if nav_key(&key).is_none() && media_key(&key).is_none() {
                let keys: Vec<&str> = NAV_KEYS.iter().map(|(k, _)| *k).chain(MEDIA_KEYS.iter().map(|(k, _)| *k)).collect();
                failed(&format!("Unknown key {}. Keys: {}", py_repr_str(&raw), keys.join(", ")))
            } else {
                let times = a.get_one::<i64>("times").copied().unwrap_or(1);
                tv.control(&key, ControlArgs { repeat: Some(times), ..Default::default() }).await
            }
        }
        "volume" => {
            let lv = s("level").to_lowercase();
            if lv == "up" || lv == "down" {
                let change = if lv == "up" { VOLUME_STEP } else { -VOLUME_STEP };
                control("volume", ControlArgs { volume_change: Some(change), ..Default::default() }).await
            } else {
                match pyfmt::parse_decimal(lv.trim_end_matches('%')) {
                    None => failed("Give a level 0-100, up or down."),
                    Some(level) => control("volume", ControlArgs { volume: Some(level), ..Default::default() }).await,
                }
            }
        }
        "mute" => control("mute", ControlArgs::default()).await,
        "unmute" => control("unmute", ControlArgs::default()).await,
        "open" => control("open_app", ControlArgs { app: words("app"), ..Default::default() }).await,
        "youtube" => control("youtube_search", ControlArgs { query: words("query"), ..Default::default() }).await,
        "type" => control("type_text", ControlArgs { text: words("text"), ..Default::default() }).await,
        "screenshot" => {
            let dest = s("out");
            let dest = if dest.is_empty() { None } else { Some(PathBuf::from(dest)) };
            match tv.screenshot(None, dest.as_deref()).await {
                Ok(shot) => {
                    let line =
                        if is_ok(&shot) { format!("Saved {}", get_str(&shot, "path")) } else { get_str(&shot, "line") };
                    let mut shot = shot;
                    shot.insert("line".into(), Value::String(line));
                    shot
                }
                Err(e) => failed(&format!("Could not save the picture ({e}).")),
            }
        }
        "apps" => {
            let have: BTreeSet<String> = tv.packages().await.into_iter().collect();
            if have.is_empty() {
                failed(UNREACHABLE_LINE)
            } else {
                let names: Vec<&str> = APPS.iter().filter(|x| have.contains(x.package())).map(|x| x.name).collect();
                let line = if names.is_empty() {
                    "None of the apps eve knows by name is on the TV.".to_string()
                } else {
                    format!("On the TV: {}.", names.join(", "))
                };
                obj(vec![("ok", Value::Bool(true)), ("line", Value::String(line))])
            }
        }
        _ => failed("Unknown command."),
    };
    let line = get_str(&result, "line");
    if !line.is_empty() {
        for l in pyfmt::splitlines(&line) {
            out(l);
        }
    }
    if is_ok(&result) {
        0
    } else {
        1
    }
}

#[cfg(test)]
mod tests;
