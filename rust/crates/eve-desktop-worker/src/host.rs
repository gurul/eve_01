//! The exec host: the Python child that holds the model's namespace (see `exec_host.py`).
//!
//! `exec_py` is Python by contract (the planner writes Python against PyAutoGUI), so the one piece of the desktop
//! worker that cannot be Rust is the interpreter the model's code runs in. [`Namespace`] owns that child: the
//! persistent globals, PyAutoGUI and Pillow live there; everything else (the output list and its limits, the
//! helpers, the auto-screenshot rule, the daemon's line protocol) is this crate.
//!
//! The pipe carries one JSON object per line. A request from Rust is `{"op": …}` and is answered by
//! `{"t": "reply", "ok" | "err"}`. While the host runs the model's code it sends `{"t": "log" | "display" | "call" |
//! "act" | "act_end"}` and waits for `{"ok"}` / `{"err": [kind, message]}`, then `{"t": "done"}` ends the call. A
//! helper running here may drive PyAutoGUI meanwhile ([`HostGui`]): the host serves that nested `{"op": "gui"}`
//! while it waits.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_desktop_lanes::desktop_helpers::Gui;
use eve_desktop_lanes::LaneError;

use crate::Output;

/// The host's source, run with `python -I -c` under the file name [`EXEC_HOST_NAME`] (what tracebacks show).
pub const EXEC_HOST: &str = include_str!("exec_host.py");
pub const EXEC_HOST_NAME: &str = "eve-desktop-worker/exec_host.py";
const BOOT: &str = "import sys; exec(compile(sys.argv[2], sys.argv[1], 'exec'))";

/// Where the Python for the exec host is named: an interpreter with PyAutoGUI and Pillow.
pub const PYTHON_ENV: &str = "EVE_WORKER_PYTHON";

/// The Python the exec host runs on: EVE_WORKER_PYTHON, else the bridge checkout's venv (the interpreter the Python
/// daemon starts its worker with), else `python3`.
pub fn default_python() -> PathBuf {
    if let Some(p) = std::env::var_os(PYTHON_ENV).filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    let venv = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/.venv/bin/python3"));
    if venv.exists() {
        return venv;
    }
    PathBuf::from("python3")
}

/// The exec host stopped answering: it exited, or wrote something that is not a protocol line. The worker cannot
/// go on (the model's namespace is gone), as the Python worker could not after its own interpreter died.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the exec host is gone: {0}")]
pub struct HostDied(pub String);

struct Io {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

struct Link {
    io: Mutex<Io>,
    child: Mutex<Child>,
    /// the output of the exec call in flight, for a log the host sends while it serves a nested request
    sink: Mutex<Option<Arc<Mutex<Output>>>>,
    platform: Mutex<String>,
    executable: Mutex<String>,
}

impl Drop for Link {
    fn drop(&mut self) {
        let mut child = self.child.lock();
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// The model's exec namespace: a handle on the exec host. Cheap to clone; the host is killed when the last handle
/// goes.
#[derive(Clone)]
pub struct Namespace {
    link: Arc<Link>,
}

impl std::fmt::Debug for Namespace {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Namespace").field("platform", &*self.link.platform.lock()).finish_non_exhaustive()
    }
}

impl Namespace {
    /// Start the exec host on `python` with an empty namespace (`{"__builtins__": builtins}`). stderr is inherited, so
    /// the host's own tracebacks land in the daemon log as the Python worker's did.
    pub fn spawn(python: &Path) -> anyhow::Result<Namespace> {
        let mut child = Command::new(python)
            .arg("-I")
            .arg("-c")
            .arg(BOOT)
            .arg(EXEC_HOST_NAME)
            .arg(EXEC_HOST)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|e| {
                let kind = if e.kind() == std::io::ErrorKind::NotFound { "FileNotFoundError" } else { "OSError" };
                LaneError::err(kind, format!("{e}: '{}'", python.display()))
            })?;
        let stdin = child.stdin.take().ok_or_else(|| HostDied("no stdin".into()))?;
        let stdout = BufReader::new(child.stdout.take().ok_or_else(|| HostDied("no stdout".into()))?);
        let ns = Namespace {
            link: Arc::new(Link {
                io: Mutex::new(Io { stdin, stdout }),
                child: Mutex::new(child),
                sink: Mutex::new(None),
                platform: Mutex::new(String::new()),
                executable: Mutex::new(String::new()),
            }),
        };
        let hello = ns.recv()?;
        if hello.get("t").and_then(Value::as_str) != Some("hello") {
            return Err(HostDied(format!("unexpected first line {hello}")).into());
        }
        Ok(ns)
    }

    /// Import `modules` into the namespace by their own names (the worker's `{"pyautogui": …, "time": …}`), and
    /// learn the host's `sys.platform` and interpreter path.
    pub fn init(&self, modules: &[&str]) -> anyhow::Result<()> {
        let reply = self.request(json!({"op": "init", "modules": modules}))?;
        *self.link.platform.lock() = reply.get("platform").and_then(Value::as_str).unwrap_or_default().to_string();
        *self.link.executable.lock() =
            reply.get("executable").and_then(Value::as_str).unwrap_or_default().to_string();
        Ok(())
    }

    /// The host interpreter's `sys.platform` (empty before [`Namespace::init`]).
    pub fn platform(&self) -> String {
        self.link.platform.lock().clone()
    }

    /// `os.path.realpath(sys.executable)` of the host interpreter (empty before [`Namespace::init`]).
    pub fn executable(&self) -> String {
        self.link.executable.lock().clone()
    }

    /// Run the worker's own setup statements in the namespace (not the model's code: nothing is captured).
    pub fn setup(&self, code: &str) -> anyhow::Result<()> {
        self.request(json!({"op": "setup", "code": code})).map(|_| ())
    }

    /// Evaluate `expr` in the namespace and return it as JSON (tuples as lists). The tests read their fake PyAutoGUI
    /// with it.
    pub fn eval(&self, expr: &str) -> anyhow::Result<Value> {
        self.request(json!({"op": "eval", "expr": expr}))
    }

    /// Whether `name` is bound in the namespace.
    pub fn contains(&self, name: &str) -> anyhow::Result<bool> {
        let expr = format!("{} in globals()", eve_decider::py_str_repr(name));
        Ok(self.eval(&expr)?.as_bool().unwrap_or(false))
    }

    /// One PyAutoGUI call (`pyautogui.<fn>(*args, **kwargs)`, unwrapped by the acting wrap). `size` answers `[w, h]`.
    pub fn gui_call(&self, function: &str, args: Value, kwargs: Value) -> anyhow::Result<Value> {
        self.request(json!({"op": "gui", "fn": function, "args": args, "kwargs": kwargs}))
    }

    /// Send one request and wait for its reply. A log the host sends meanwhile (the model's print redirected while a
    /// helper drives PyAutoGUI) goes to the exec call's output.
    pub fn request(&self, op: Value) -> anyhow::Result<Value> {
        self.send(&op)?;
        loop {
            let msg = self.recv()?;
            match msg.get("t").and_then(Value::as_str) {
                Some("reply") => {
                    if let Some(err) = msg.get("err") {
                        return Err(LaneError::err(err_part(err, 0), err_part(err, 1)));
                    }
                    return Ok(msg.get("ok").cloned().unwrap_or(Value::Null));
                }
                Some("log") | Some("display") => {
                    let sink = self.link.sink.lock().clone();
                    let reply = match sink {
                        Some(out) => out.lock().handle(&msg),
                        None => json!({"ok": null}),
                    };
                    self.send(&reply)?;
                }
                Some("done") => return Err(HostDied(format!("an exec call ended inside a request: {msg}")).into()),
                _ => {
                    self.send(&json!({"err": ["RuntimeError", "the desktop worker is busy"]}))?;
                }
            }
        }
    }

    pub(crate) fn set_sink(&self, out: Option<Arc<Mutex<Output>>>) {
        *self.link.sink.lock() = out;
    }

    pub(crate) fn send(&self, msg: &Value) -> anyhow::Result<()> {
        let mut io = self.link.io.lock();
        let mut line = serde_json::to_string(msg)?;
        line.push('\n');
        io.stdin.write_all(line.as_bytes()).map_err(|e| HostDied(e.to_string()))?;
        io.stdin.flush().map_err(|e| HostDied(e.to_string()))?;
        Ok(())
    }

    pub(crate) fn recv(&self) -> anyhow::Result<Value> {
        let mut io = self.link.io.lock();
        let mut line = String::new();
        let n = io.stdout.read_line(&mut line).map_err(|e| HostDied(e.to_string()))?;
        if n == 0 {
            drop(io);
            let status = self.link.child.lock().try_wait().ok().flatten();
            let how = status.map(|s| s.to_string()).unwrap_or_else(|| "closed its pipe".to_string());
            return Err(HostDied(how).into());
        }
        match serde_json::from_str(&line) {
            Ok(v) => Ok(v),
            // Python strings may hold lone surrogates (json.dumps writes them as \udXXX escapes); a Rust string
            // cannot, so they read as U+FFFD.
            Err(_) => serde_json::from_str(&replace_lone_surrogates(&line))
                .map_err(|e| HostDied(format!("not a protocol line ({e})")).into()),
        }
    }
}

/// `\uD800`–`\uDFFF` escapes that are not a high + low pair, as `�`. The host writes ASCII JSON
/// (`ensure_ascii`), so every non-ASCII character is an escape here.
pub(crate) fn replace_lone_surrogates(line: &str) -> String {
    fn escape_at(b: &[u8], i: usize) -> Option<u16> {
        if i + 6 <= b.len() && b[i] == b'\\' && b[i + 1] == b'u' {
            std::str::from_utf8(&b[i + 2..i + 6]).ok().and_then(|h| u16::from_str_radix(h, 16).ok())
        } else {
            None
        }
    }
    let b = line.as_bytes();
    let mut out = String::with_capacity(line.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' {
            if let Some(u) = escape_at(b, i) {
                if (0xD800..0xDC00).contains(&u) {
                    if escape_at(b, i + 6).is_some_and(|lo| (0xDC00..0xE000).contains(&lo)) {
                        out.push_str(&line[i..i + 12]);
                        i += 12;
                    } else {
                        out.push_str("\\ufffd");
                        i += 6;
                    }
                    continue;
                }
                if (0xDC00..0xE000).contains(&u) {
                    out.push_str("\\ufffd");
                    i += 6;
                    continue;
                }
                out.push_str(&line[i..i + 6]);
                i += 6;
                continue;
            }
            // any other escape: copy it and the escaped character (so `\\u…` stays literal text)
            let end = (i + 2).min(b.len());
            out.push_str(&line[i..end]);
            i = end;
            continue;
        }
        let ch = line[i..].chars().next().unwrap_or('\u{fffd}');
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn err_part(err: &Value, i: usize) -> String {
    match err.get(i) {
        Some(Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    }
}

/// The helpers' PyAutoGUI: each call runs in the exec host (`pyautogui.click(x, y, clicks=n)`, `hotkey(*keys)`,
/// `press(key)`, `size()`), so the fail-safe, PAUSE and the real key map are PyAutoGUI's own.
pub struct HostGui {
    ns: Namespace,
}

impl HostGui {
    pub fn new(ns: &Namespace) -> Self {
        HostGui { ns: ns.clone() }
    }
}

impl Gui for HostGui {
    fn size(&self) -> (i64, i64) {
        match self.ns.gui_call("size", json!([]), json!({})) {
            Ok(Value::Array(v)) if v.len() == 2 => (v[0].as_i64().unwrap_or(0), v[1].as_i64().unwrap_or(0)),
            Ok(other) => {
                tracing::warn!("desktop worker: pyautogui.size() answered {other}");
                (0, 0)
            }
            Err(e) => {
                tracing::warn!("desktop worker: pyautogui.size() failed: {e}");
                (0, 0)
            }
        }
    }

    fn click(&mut self, x: i64, y: i64, clicks: u32) -> anyhow::Result<()> {
        self.ns.gui_call("click", json!([x, y]), json!({"clicks": clicks})).map(|_| ())
    }

    fn hotkey(&mut self, keys: &[&str]) -> anyhow::Result<()> {
        self.ns.gui_call("hotkey", json!(keys), json!({})).map(|_| ())
    }

    fn press(&mut self, key: &str) -> anyhow::Result<()> {
        self.ns.gui_call("press", json!([key]), json!({})).map(|_| ())
    }
}
