//! What the service backends reach outside themselves: `shutil.which`, `subprocess.run(..., capture_output=True)`,
//! `print` to stdout and stderr, and `os.getuid`. [`SystemHost`] is the real one; tests pass a recorder, as the
//! Python tests monkeypatch `shutil.which` and `subprocess.run` and read `capsys`.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `subprocess.CompletedProcess` with `capture_output=True`: the exit code (a signal is `-signal`, as Python
/// reports it) and the captured streams.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Completed {
    pub returncode: i32,
    pub stdout: String,
    pub stderr: String,
}

impl Completed {
    pub fn new(returncode: i32, stdout: impl Into<String>, stderr: impl Into<String>) -> Completed {
        Completed { returncode, stdout: stdout.into(), stderr: stderr.into() }
    }
}

/// The process boundary of the service backends.
pub trait Host: Send + Sync {
    /// `shutil.which(name)`.
    fn which(&self, name: &str) -> Option<PathBuf>;
    /// `subprocess.run(argv, capture_output=True)`. Err where Python raises (the program cannot be started).
    fn run(&self, argv: &[String]) -> io::Result<Completed>;
    /// `print(line)`.
    fn print(&self, line: &str);
    /// `print(line, file=sys.stderr)`.
    fn eprint(&self, line: &str);
    /// `os.getuid()`. Err off Unix, where Python's `os` has no `getuid`.
    fn getuid(&self) -> io::Result<u32>;
}

/// The real host: PATH lookup, child processes, this process's stdout and stderr.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemHost;

impl Host for SystemHost {
    fn which(&self, name: &str) -> Option<PathBuf> {
        which::which(name).ok()
    }

    fn run(&self, argv: &[String]) -> io::Result<Completed> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "empty command"))?;
        // subprocess.run leaves stdin inherited; Command::output would close it.
        let out = Command::new(program).args(args).stdin(Stdio::inherit()).output()?;
        Ok(Completed {
            returncode: exit_code(&out.status),
            stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        })
    }

    fn print(&self, line: &str) {
        let mut out = io::stdout().lock();
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }

    fn eprint(&self, line: &str) {
        let _ = writeln!(io::stderr().lock(), "{line}");
    }

    fn getuid(&self) -> io::Result<u32> {
        #[cfg(unix)]
        {
            // SAFETY: getuid has no preconditions and cannot fail.
            Ok(unsafe { libc::getuid() })
        }
        #[cfg(not(unix))]
        {
            Err(io::Error::new(io::ErrorKind::Unsupported, "module 'os' has no attribute 'getuid'"))
        }
    }
}

fn exit_code(status: &std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return -sig;
        }
    }
    -1
}

/// How the unit starts eve. Python's units name the interpreter running the install (`sys.executable`) with
/// `-m eve_bridge.cli <subcommand>`; a Rust daemon has no interpreter, so the caller says which program to start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// `<python> -m eve_bridge.cli <subcommand>`: Python's line, byte for byte.
    Python(String),
    /// `<exe> <subcommand>`: this Rust build's own `eve-bridge` binary.
    Native(String),
}

impl Launch {
    /// The binary running this install (`std::env::current_exe`), as Python names `sys.executable`; an empty
    /// string when it cannot be found, as `sys.executable` is.
    pub fn current() -> Launch {
        Launch::Native(
            std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default(),
        )
    }

    /// The program the unit starts.
    pub fn program(&self) -> &str {
        match self {
            Launch::Python(p) | Launch::Native(p) => p,
        }
    }

    /// The arguments after the program.
    pub fn args(&self, subcommand: &str) -> Vec<String> {
        match self {
            Launch::Python(_) => vec!["-m".into(), "eve_bridge.cli".into(), subcommand.into()],
            Launch::Native(_) => vec![subcommand.into()],
        }
    }

    /// `ProgramArguments`: the program, then its arguments.
    pub fn argv(&self, subcommand: &str) -> Vec<String> {
        let mut argv = vec![self.program().to_string()];
        argv.extend(self.args(subcommand));
        argv
    }
}

/// `str(path)` for a path that must go into a UTF-8 file: Python's encode would raise on one that is not.
pub(crate) fn path_str(path: &Path) -> anyhow::Result<&str> {
    path.to_str().ok_or_else(|| anyhow::anyhow!("path is not valid UTF-8: {}", path.display()))
}

/// Python's `str.strip()` with no argument.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// Python's `str.splitlines()`.
pub(crate) fn py_splitlines(s: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut chars = s.char_indices().peekable();
    while let Some((i, c)) = chars.next() {
        let brk = matches!(
            c,
            '\n' | '\r' | '\x0b' | '\x0c' | '\x1c' | '\x1d' | '\x1e' | '\u{85}' | '\u{2028}' | '\u{2029}'
        );
        if !brk {
            continue;
        }
        out.push(&s[start..i]);
        let mut end = i + c.len_utf8();
        if c == '\r' {
            if let Some(&(j, '\n')) = chars.peek() {
                chars.next();
                end = j + 1;
            }
        }
        start = end;
    }
    if start < s.len() {
        out.push(&s[start..]);
    }
    out
}

/// The name of the `OSError` subclass Python raises for `err` (`type(e).__name__`).
pub(crate) fn os_error_name(err: &io::Error) -> &'static str {
    let Some(code) = err.raw_os_error() else { return "OSError" };
    #[cfg(unix)]
    {
        match code {
            libc::EEXIST => "FileExistsError",
            libc::ENOENT => "FileNotFoundError",
            libc::EACCES | libc::EPERM => "PermissionError",
            libc::ENOTDIR => "NotADirectoryError",
            libc::EISDIR => "IsADirectoryError",
            libc::EINTR => "InterruptedError",
            libc::EAGAIN | libc::EALREADY | libc::EINPROGRESS => "BlockingIOError",
            libc::ECHILD => "ChildProcessError",
            libc::EPIPE | libc::ESHUTDOWN => "BrokenPipeError",
            libc::ECONNABORTED => "ConnectionAbortedError",
            libc::ECONNREFUSED => "ConnectionRefusedError",
            libc::ECONNRESET => "ConnectionResetError",
            libc::ESRCH => "ProcessLookupError",
            libc::ETIMEDOUT => "TimeoutError",
            _ => "OSError",
        }
    }
    #[cfg(not(unix))]
    {
        match err.kind() {
            io::ErrorKind::AlreadyExists => "FileExistsError",
            io::ErrorKind::NotFound => "FileNotFoundError",
            io::ErrorKind::PermissionDenied => "PermissionError",
            _ => {
                let _ = code;
                "OSError"
            }
        }
    }
}

/// `os.pathsep`.
pub(crate) const PATHSEP: &str = if cfg!(windows) { ";" } else { ":" };

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splitlines_as_python() {
        assert_eq!(py_splitlines("a\nb\r\nc\rd\x0be"), vec!["a", "b", "c", "d", "e"]);
        assert_eq!(py_splitlines("a\n"), vec!["a"]);
        assert_eq!(py_splitlines(""), Vec::<&str>::new());
        assert_eq!(py_splitlines("\n\nx"), vec!["", "", "x"]);
    }

    #[test]
    fn launch_lines() {
        assert_eq!(Launch::Python("/py".into()).argv("daemon"), ["/py", "-m", "eve_bridge.cli", "daemon"]);
        assert_eq!(Launch::Native("/bin/eve-bridge".into()).argv("notes-widget"), ["/bin/eve-bridge", "notes-widget"]);
    }

    #[cfg(unix)]
    #[test]
    fn errno_names() {
        assert_eq!(os_error_name(&io::Error::from_raw_os_error(libc::EEXIST)), "FileExistsError");
        assert_eq!(os_error_name(&io::Error::from_raw_os_error(libc::EROFS)), "OSError");
        assert_eq!(os_error_name(&io::Error::other("x")), "OSError");
    }
}
