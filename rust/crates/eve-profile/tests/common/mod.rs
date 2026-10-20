//! What the profile tests share: a fresh process for a case (Python's `subprocess.run([sys.executable, script])`),
//! since an installed firewall stays for the life of a process, and one log capture for a single thread.
#![allow(dead_code)]

use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use serde_json::Value;

pub const CANARY: &str = "CANARY-NOT-A-KEY-7f3c";
/// Names the case a re-run test binary is to act out; any other run of that test returns at once.
pub const CHILD: &str = "EVE_PROFILE_TEST_CHILD";

/// Whether this process was started to act out the case `name`.
pub fn is_child(name: &str) -> bool {
    std::env::var(CHILD).as_deref() == Ok(name)
}

/// What a fresh process printed: its result line (parsed), its whole stdout and its stderr (the log).
pub struct Ran {
    pub result: Value,
    pub stdout: String,
    pub log: String,
}

/// Run this test binary again, with only `env` (and CHILD) as its environment, to act out the test `name`: the case
/// prints one line `RESULT:<json>`. Asserts the process exited 0.
pub fn run_child(name: &str, env: &[(&str, String)], cwd: Option<&Path>) -> Ran {
    let exe = std::env::current_exe().expect("the test binary");
    let mut cmd = Command::new(exe);
    cmd.args([name, "--exact", "--nocapture", "--test-threads=1"]).env_clear().env(CHILD, name);
    for (k, v) in env {
        cmd.env(k, v);
    }
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    let done = cmd.output().expect("the case ran");
    let stdout = String::from_utf8_lossy(&done.stdout).into_owned();
    let log = String::from_utf8_lossy(&done.stderr).into_owned();
    assert!(done.status.success(), "case {name} failed: {stdout}\n{log}");
    let at = stdout.find("RESULT:").unwrap_or_else(|| panic!("case {name} printed no result: {stdout}\n{log}"));
    let line = stdout[at + "RESULT:".len()..].lines().next().unwrap_or("");
    let result = serde_json::from_str(line).unwrap_or_else(|e| panic!("case {name}: {e}: {line}"));
    Ran { result, stdout, log }
}

/// The case's one result line.
pub fn print_result(value: &Value) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "\nRESULT:{value}").unwrap();
    out.flush().unwrap();
}

/// In a case: the log goes to stderr as "<target>: <message>" lines (the Python harness's "%(name)s: %(message)s").
pub fn log_to_stderr() {
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .without_time()
        .with_level(false)
        .with_target(true)
        .with_max_level(tracing::Level::INFO)
        .try_init();
}

#[derive(Clone, Default)]
pub struct Buffer(pub Arc<Mutex<Vec<u8>>>);

impl Write for Buffer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Run `f` with this thread's log lines captured (pytest's caplog): what it returned, and the lines.
pub fn capture_log<T>(f: impl FnOnce() -> T) -> (T, Vec<String>) {
    let buffer = Buffer::default();
    let writer = buffer.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .without_time()
        .with_level(false)
        .with_target(false)
        .with_max_level(tracing::Level::INFO)
        .finish();
    let out = tracing::subscriber::with_default(subscriber, f);
    let text = String::from_utf8_lossy(&buffer.0.lock().unwrap()).into_owned();
    (out, text.lines().map(str::to_string).collect())
}

/// A program file with the execute bit.
pub fn write_program(path: &Path, text: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
