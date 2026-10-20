//! What cli.py's commands write to and ask through: stdout and stderr (`print`), the daemon's IPC socket
//! (`hooks._client.post`) and the terminal (`input`). The real ones in the binary; tests lend their own.

use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex};

use serde_json::Value;

pub type Say = Box<dyn Fn(&str) + Send + Sync>;
/// `post(event, socket_path=, timeout=, connect_timeout=)`.
pub type Post = Box<dyn Fn(&Value, Option<&str>, f64, f64) -> Option<Value> + Send + Sync>;
/// `input(prompt)`: Err(UnexpectedEof) where Python raises EOFError.
pub type Ask = Box<dyn Fn(&str) -> io::Result<String> + Send + Sync>;

pub struct Io {
    pub out: Say,
    pub err: Say,
    pub post: Post,
    pub ask: Ask,
    /// `sys.stdin.isatty()`
    pub stdin_tty: bool,
}

impl Io {
    /// This process: stdout, stderr, the daemon's socket and the terminal.
    pub fn process() -> Io {
        Io {
            out: Box::new(|line| {
                let mut o = io::stdout().lock();
                let _ = writeln!(o, "{line}");
                let _ = o.flush();
            }),
            err: Box::new(|line| {
                let _ = writeln!(io::stderr().lock(), "{line}");
            }),
            post: Box::new(|event, sock, timeout, connect| eve_hooks::client::post(event, sock, timeout, connect)),
            ask: Box::new(|prompt| eve_vault::input(prompt)),
            stdin_tty: io::stdin().is_terminal(),
        }
    }

    /// `print(line)`.
    pub fn say(&self, line: &str) {
        (self.out)(line)
    }

    /// `print(line, file=sys.stderr)`.
    pub fn warn(&self, line: &str) {
        (self.err)(line)
    }

    /// `post(...)` with Python's default connect timeout (DEFAULT_TIMEOUT_SECS).
    pub fn post(&self, event: &Value, socket: Option<&str>, timeout: f64) -> Option<Value> {
        (self.post)(event, socket, timeout, eve_hooks::client::DEFAULT_TIMEOUT_SECS)
    }

    /// `input(prompt)`.
    pub fn ask(&self, prompt: &str) -> io::Result<String> {
        (self.ask)(prompt)
    }
}

/// What a test's [`Io`] printed.
#[derive(Debug, Default, Clone)]
pub struct Captured {
    pub out: String,
    pub err: String,
}

/// An [`Io`] that keeps what is printed, posts with `post` and answers `ask` with `answer`.
pub fn capture(post: Post, answer: Option<String>) -> (Io, Arc<Mutex<Captured>>) {
    let seen = Arc::new(Mutex::new(Captured::default()));
    let (o, e) = (seen.clone(), seen.clone());
    let io = Io {
        out: Box::new(move |line| {
            let mut c = o.lock().unwrap_or_else(|p| p.into_inner());
            c.out.push_str(line);
            c.out.push('\n');
        }),
        err: Box::new(move |line| {
            let mut c = e.lock().unwrap_or_else(|p| p.into_inner());
            c.err.push_str(line);
            c.err.push('\n');
        }),
        post,
        ask: Box::new(move |_prompt| match &answer {
            Some(a) => Ok(a.clone()),
            None => Err(io::Error::new(io::ErrorKind::UnexpectedEof, "EOF when reading a line")),
        }),
        stdin_tty: false,
    };
    (io, seen)
}

impl Captured {
    /// `capsys.readouterr()`: what was printed since the last read.
    pub fn take(seen: &Arc<Mutex<Captured>>) -> Captured {
        std::mem::take(&mut *seen.lock().unwrap_or_else(|p| p.into_inner()))
    }
}
