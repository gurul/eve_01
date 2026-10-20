//! The app-server child: `asyncio.create_subprocess_exec(binary, 'app-server', '--listen', 'stdio://', env=env,
//! stdin=PIPE, stdout=PIPE, stderr=DEVNULL)`, behind a seam so tests can stand in a fake server (Python's tests
//! patch `asyncio.create_subprocess_exec`).

use std::io;
use std::process::Stdio;
use std::sync::Arc;

use futures::future::BoxFuture;
use indexmap::IndexMap;
use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::watch;

/// What the agent needs of the child beyond its pipes: `returncode`, `terminate()`, `kill()`, `wait()`.
pub trait Child: Send + Sync {
    /// `Process.returncode`: `None` while it runs; a signal is its negative number, as asyncio reports it.
    fn returncode(&self) -> Option<i32>;
    /// `Process.terminate()` (SIGTERM).
    fn terminate(&self);
    /// `Process.kill()` (SIGKILL).
    fn kill(&self);
    /// `await Process.wait()`.
    fn wait(&self) -> BoxFuture<'_, i32>;
}

/// A started app-server: its stdin, its stdout, and the process.
pub struct Spawned {
    pub stdin: Box<dyn AsyncWrite + Send + Unpin>,
    pub stdout: Box<dyn AsyncRead + Send + Unpin>,
    pub child: Arc<dyn Child>,
}

/// Starts `program` with `args` and exactly the environment `env` (nothing inherited).
pub type Spawner =
    Arc<dyn Fn(String, Vec<String>, IndexMap<String, String>) -> BoxFuture<'static, io::Result<Spawned>> + Send + Sync>;

/// The real spawner: a child process with piped stdin/stdout and stderr to /dev/null.
pub fn os_spawner() -> Spawner {
    Arc::new(|program, args, env| Box::pin(async move { spawn(&program, &args, &env) }))
}

struct OsChild {
    pid: Option<u32>,
    code: Mutex<Option<i32>>,
    exited: watch::Receiver<Option<i32>>,
}

fn status_code(status: &std::process::ExitStatus) -> i32 {
    use std::os::unix::process::ExitStatusExt as _;
    status.code().or_else(|| status.signal().map(|s| -s)).unwrap_or(-1)
}

fn spawn(program: &str, args: &[String], env: &IndexMap<String, String>) -> io::Result<Spawned> {
    let mut child = tokio::process::Command::new(program)
        .args(args)
        .env_clear()
        .envs(env)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let stdin = child.stdin.take().ok_or_else(|| io::Error::other("no stdin pipe"))?;
    let stdout = child.stdout.take().ok_or_else(|| io::Error::other("no stdout pipe"))?;
    let pid = child.id();
    let (tx, rx) = watch::channel(None);
    // asyncio's child watcher reaps the process in the background and sets `returncode`; so does this task.
    tokio::spawn(async move {
        let code = match child.wait().await {
            Ok(status) => status_code(&status),
            Err(_) => -1,
        };
        let _ = tx.send(Some(code));
    });
    Ok(Spawned {
        stdin: Box::new(stdin),
        stdout: Box::new(stdout),
        child: Arc::new(OsChild { pid, code: Mutex::new(None), exited: rx }),
    })
}

impl OsChild {
    fn signal(&self, sig: i32) {
        if self.returncode().is_some() {
            return; // reaped: the pid may belong to another process now
        }
        if let Some(pid) = self.pid.and_then(|p| i32::try_from(p).ok()) {
            // SAFETY: kill(2) with a pid this process started and has not reaped; no memory is touched.
            unsafe {
                libc::kill(pid, sig);
            }
        }
    }
}

impl Child for OsChild {
    fn returncode(&self) -> Option<i32> {
        let mut code = self.code.lock();
        if code.is_none() {
            *code = *self.exited.borrow();
        }
        *code
    }

    fn terminate(&self) {
        self.signal(libc::SIGTERM);
    }

    fn kill(&self) {
        self.signal(libc::SIGKILL);
    }

    fn wait(&self) -> BoxFuture<'_, i32> {
        let mut rx = self.exited.clone();
        Box::pin(async move {
            match rx.wait_for(Option::is_some).await {
                Ok(code) => code.unwrap_or(-1),
                Err(_) => self.returncode().unwrap_or(-1),
            }
        })
    }
}
