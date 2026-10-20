//! Muse on the owner's Raspberry Pi: one question in, Muse's answer out (docs/muse.md).
//!
//! The Pi runs Meta's Muse Linux Device SDK with eve's relay (pi/muse/). eve reaches it over SSH, key login
//! only, and runs the relay's `ask.py` with the message on stdin; `ask.py` prints one JSON line,
//! `{"ok", "reply"}` or `{"ok": false, "error"}`. The Pi keeps the Muse connection; nothing Muse-side runs on
//! the Mac.
//!
//! `EVE_MUSE_PI` is the SSH target (`user@host`); unset, Muse is off. eve's chat with Muse is one Muse side
//! chat, its id kept in `muse-session` under the config directory, so Muse remembers the conversation across
//! turns.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::LazyLock;
use std::time::Duration;

use async_trait::async_trait;
use regex::Regex;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;

pub const ENV: &str = "EVE_MUSE_PI";
pub const ASK: &str = "/opt/eve-muse/venv-python /opt/eve-muse/ask.py";
pub const SESSION_FILE: &str = "muse-session";
/// The Pi waits up to 120 s for Muse; SSH and the socket get the rest.
pub const ANSWER_SECS: f64 = 170.0;
pub const CONNECT_SECS: u32 = 6;

pub const UNREACHABLE_LINE: &str = "I couldn't reach Muse on the Pi just now.";
pub const OFF_LINE: &str = "Muse is off on this Mac (EVE_MUSE_PI).";
pub const NO_ANSWER_LINE: &str = "Muse didn't answer that.";
pub const NOT_CONNECTED_LINE: &str = "Muse isn't connected on the Pi right now.";
pub const LATE_LINE: &str = "Muse didn't finish answering in time.";

static TARGET: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9._-]+@[A-Za-z0-9.:_-]+$").expect("target regex"));
static SESSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").expect("session regex")
});

/// `~/.config/eve-bridge` (Python's `CONFIG_DIR`).
pub fn config_dir() -> PathBuf {
    dirs::home_dir().unwrap_or_default().join(".config").join("eve-bridge")
}

/// Muse's answer, or a short line saying why there is none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    pub ok: bool,
    pub text: String,
}

impl Answer {
    pub fn new(ok: bool, text: impl Into<String>) -> Self {
        Answer { ok, text: text.into() }
    }
}

/// The SSH target from EVE_MUSE_PI, or "" when unset or not a plain user@host.
pub fn target() -> String {
    target_from(std::env::var(ENV).ok().as_deref())
}

/// `target` over a given value of `EVE_MUSE_PI` (None = unset).
pub fn target_from(value: Option<&str>) -> String {
    let value = py_strip(value.unwrap_or(""));
    if TARGET.is_match(value) {
        value.to_string()
    } else {
        String::new()
    }
}

pub fn available() -> bool {
    !target().is_empty()
}

/// eve's Muse side chat: made once, then kept, so Muse keeps the thread.
///
/// `directory` defaults to [`config_dir`]. A missing, unreadable or malformed file is replaced with a fresh
/// uuid4; failing to write it is an error (Python raises `OSError`).
pub fn session_id(directory: Option<&Path>) -> io::Result<String> {
    let dir = directory.map(Path::to_path_buf).unwrap_or_else(config_dir);
    let path = dir.join(SESSION_FILE);
    if let Ok(bytes) = std::fs::read(&path) {
        if let Ok(text) = std::str::from_utf8(&bytes) {
            let value = py_strip(text);
            if SESSION.is_match(value) {
                return Ok(value.to_string());
            }
        }
    }
    let value = uuid::Uuid::new_v4().to_string();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, format!("{value}\n"))?;
    Ok(value)
}

/// The ssh argv: key login only, a short connect timeout, keepalives, and the relay's `ask.py`.
pub fn command(host: &str, session: &str) -> Vec<String> {
    vec![
        "ssh".into(),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        format!("ConnectTimeout={CONNECT_SECS}"),
        "-o".into(),
        "ServerAliveInterval=30".into(),
        host.into(),
        format!("{ASK} --session-id {session}"),
    ]
}

/// Python's `str.splitlines()` (the separators it splits on, no keepends).
fn splitlines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut chars = text.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let is_break = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !is_break {
            continue;
        }
        lines.push(&text[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = chars.peek() {
                chars.next();
                end = j + 1;
            }
        }
        start = end;
    }
    if start < text.len() {
        lines.push(&text[start..]);
    }
    lines
}

/// Python's `str.strip()`: `str.isspace` also counts the separators U+001C..U+001F.
fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\x1c'..='\x1f').contains(&c))
}

/// Python truthiness of a JSON value.
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// `str(value or "")`, as far as the substring checks in `parse` can tell.
fn error_text(value: Option<&Value>) -> String {
    if !truthy(value) {
        return String::new();
    }
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(other) => other.to_string(),
        None => String::new(),
    }
}

/// `ask.py`'s last line: Muse's reply, or why there is none (never the raw output).
pub fn parse(stdout: &[u8]) -> Answer {
    let decoded = String::from_utf8_lossy(stdout);
    let last = splitlines(&decoded).into_iter().rfind(|line| !py_strip(line).is_empty());
    let reply = last.and_then(|line| serde_json::from_str::<Value>(line).ok());
    let Some(Value::Object(reply)) = reply else {
        return Answer::new(false, UNREACHABLE_LINE);
    };
    let text = match reply.get("reply") {
        Some(Value::String(s)) => s.as_str(),
        _ => "",
    };
    let ok = truthy(reply.get("ok"));
    if ok && !py_strip(text).is_empty() {
        return Answer::new(true, py_strip(text));
    }
    if ok {
        return Answer::new(false, NO_ANSWER_LINE);
    }
    let error = error_text(reply.get("error"));
    if error.contains("not connected") {
        return Answer::new(false, NOT_CONNECTED_LINE);
    }
    if error.contains("in time") {
        let late = py_strip(text);
        return Answer::new(false, if late.is_empty() { LATE_LINE } else { late });
    }
    Answer::new(false, UNREACHABLE_LINE)
}

/// `ask`'s keyword arguments. `Default` is Python's defaults: the host from `EVE_MUSE_PI`, the kept side
/// chat, `ANSWER_SECS`.
#[derive(Debug, Clone)]
pub struct AskOptions {
    /// `host`: `None` reads [`target`]; `Some("")` means Muse is off.
    pub host: Option<String>,
    /// `session`: `None` or empty uses [`session_id`] under [`config_dir`].
    pub session: Option<String>,
    pub timeout: f64,
    /// The program run as `ssh` (found on `PATH` when it has no slash); a test seam, Python's tests put a
    /// fake `ssh` first on `PATH`.
    pub ssh: PathBuf,
}

impl Default for AskOptions {
    fn default() -> Self {
        AskOptions { host: None, session: None, timeout: ANSWER_SECS, ssh: PathBuf::from("ssh") }
    }
}

/// Python's exception class name for an `OSError` from starting a process.
fn os_error_name(e: &io::Error) -> &'static str {
    match e.kind() {
        io::ErrorKind::NotFound => "FileNotFoundError",
        io::ErrorKind::PermissionDenied => "PermissionError",
        io::ErrorKind::AlreadyExists => "FileExistsError",
        io::ErrorKind::Interrupted => "InterruptedError",
        io::ErrorKind::TimedOut => "TimeoutError",
        _ => "OSError",
    }
}

/// asyncio's `returncode`: the exit code, or minus the signal that ended the process.
fn returncode(status: &std::process::ExitStatus) -> Option<i32> {
    if let Some(code) = status.code() {
        return Some(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return Some(-sig);
        }
    }
    None
}

fn first_chars(text: &str, n: usize) -> &str {
    match text.char_indices().nth(n) {
        Some((i, _)) => &text[..i],
        None => text,
    }
}

/// One message to Muse through the Pi; Muse's answer, or a short line saying why not.
pub async fn ask(text: &str, opts: AskOptions) -> Answer {
    let host = match opts.host {
        Some(host) => host,
        None => target(),
    };
    if host.is_empty() {
        return Answer::new(false, OFF_LINE);
    }
    let session = match opts.session.filter(|s| !s.is_empty()) {
        Some(session) => Ok(session),
        None => session_id(None),
    };
    let spawned = session.and_then(|session| {
        let argv = command(&host, &session);
        Command::new(&opts.ssh)
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
    });
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            tracing::warn!("muse: ssh did not start ({})", os_error_name(&e));
            return Answer::new(false, UNREACHABLE_LINE);
        }
    };

    let mut stdin = child.stdin.take();
    let mut out = child.stdout.take();
    let mut err = child.stderr.take();
    let message = text.as_bytes().to_vec();
    let communicate = async {
        let write = async {
            if let Some(mut pipe) = stdin.take() {
                // communicate() ignores a closed pipe: the reply says what happened.
                let _ = pipe.write_all(&message).await;
                let _ = pipe.shutdown().await;
            }
        };
        let read_out = async {
            let mut buf = Vec::new();
            if let Some(pipe) = out.as_mut() {
                let _ = pipe.read_to_end(&mut buf).await;
            }
            buf
        };
        let read_err = async {
            let mut buf = Vec::new();
            if let Some(pipe) = err.as_mut() {
                let _ = pipe.read_to_end(&mut buf).await;
            }
            buf
        };
        let ((), stdout, stderr) = tokio::join!(write, read_out, read_err);
        let status = child.wait().await;
        (stdout, stderr, status)
    };
    let limit = Duration::try_from_secs_f64(opts.timeout.max(0.0)).unwrap_or(Duration::ZERO);
    let outcome = tokio::time::timeout(limit, communicate).await;
    let (stdout, stderr, status) = match outcome {
        Ok(done) => done,
        Err(_) => {
            let _ = child.kill().await;
            tracing::warn!("muse: no answer from the Pi in {:.0} s", opts.timeout);
            return Answer::new(false, LATE_LINE);
        }
    };
    let code = status.ok().and_then(|s| returncode(&s));
    let code_text = code.map_or_else(|| "None".to_string(), |c| c.to_string());
    let answer = parse(&stdout);
    tracing::info!(
        "muse: asked ({} chars), {}, ssh exit {}",
        text.chars().count(),
        if answer.ok { "answered" } else { "no answer" },
        code_text
    );
    if !answer.ok && code == Some(255) {
        let stderr = String::from_utf8_lossy(&stderr);
        tracing::warn!("muse: ssh failed: {}", first_chars(py_strip(&stderr), 200));
    }
    answer
}

/// The seam callers hold so tests can stand in for the Pi (Python's tests replace `muse_pi.ask`).
#[async_trait]
pub trait Muse: Send + Sync {
    fn available(&self) -> bool;
    async fn ask(&self, text: &str) -> Answer;
}

/// The real Muse: `EVE_MUSE_PI` and ssh, with Python's defaults.
#[derive(Debug, Clone, Copy, Default)]
pub struct PiMuse;

#[async_trait]
impl Muse for PiMuse {
    fn available(&self) -> bool {
        available()
    }

    async fn ask(&self, text: &str) -> Answer {
        ask(text, AskOptions::default()).await
    }
}

#[cfg(test)]
mod tests;
