//! Windows Task Scheduler backend for daemon auto-start. Port of `_service_windows.py`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use eve_claude_home::{HomeDirError, HomeEnv};

use crate::host::{py_strip, Host, Launch, SystemHost};

pub const NAME: &str = "task-scheduler";
pub const TASK_NAME: &str = "eve-bridge-daemon";

/// `LOG_PATH` under `home`.
pub fn log_path_under(home: &Path) -> PathBuf {
    home.join("AppData").join("Local").join("eve-bridge").join("daemon.log")
}

/// `_batch`: a value for a .cmd line: a lone % would start a variable there.
pub fn batch(value: &str) -> String {
    value.replace('%', "%%")
}

/// The Task Scheduler backend: `LOG_PATH` (fixed at import in Python), the program the task starts, and the
/// process boundary.
#[derive(Clone)]
pub struct TaskScheduler {
    pub log_path: PathBuf,
    pub launch: Launch,
    pub host: Arc<dyn Host>,
}

impl std::fmt::Debug for TaskScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskScheduler")
            .field("log_path", &self.log_path)
            .field("launch", &self.launch)
            .finish_non_exhaustive()
    }
}

impl TaskScheduler {
    /// This process: `LOG_PATH` as first fixed, this binary, the real host.
    pub fn from_process() -> Result<TaskScheduler, HomeDirError> {
        static LOG: LazyLock<Result<PathBuf, HomeDirError>> =
            LazyLock::new(|| HomeEnv::from_process().home_dir().map(|h| log_path_under(&h)));
        Ok(TaskScheduler { log_path: LOG.clone()?, launch: Launch::current(), host: Arc::new(SystemHost) })
    }

    /// `launcher_path`: the script the task runs: the daemon with its env and log redirect, in cmd syntax.
    pub fn launcher_path(&self) -> PathBuf {
        self.log_path.parent().unwrap_or(Path::new("")).join("daemon.cmd")
    }

    /// The launcher script's text: CRLF lines.
    pub fn launcher_script(&self, serial_port: Option<&str>) -> String {
        let mut lines = vec!["@echo off".to_string()];
        if let Some(port) = serial_port.filter(|p| !p.is_empty()) {
            lines.push(format!("set \"EVE_SERIAL_PORT={}\"", batch(port)));
        }
        let mut daemon = format!("\"{}\"", batch(self.launch.program()));
        for arg in self.launch.args("daemon") {
            daemon.push(' ');
            daemon.push_str(&batch(&arg));
        }
        lines.push(format!("{daemon} >> \"{}\" 2>&1", batch(&self.log_path.to_string_lossy())));
        lines.join("\r\n") + "\r\n"
    }

    /// `install`: write the launcher script and create the logon task that runs it.
    pub fn install(&self, serial_port: Option<&str>) -> anyhow::Result<i32> {
        if self.host.which("schtasks").is_none() {
            self.host.eprint("eve-bridge: `schtasks` not found on PATH");
            return Ok(2);
        }
        if let Some(parent) = self.log_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // schtasks runs /tr as a program, not through cmd.exe, so the `set ... &&` and the `>> log 2>&1` it used to
        // carry were handed to python as arguments and the task never started; /tr is also capped at 261
        // characters. The cmd syntax goes in a launcher script beside the log, and the task runs that with cmd /c.
        // Untested on Windows: built and checked by this crate's tests on any OS.
        let launcher = self.launcher_path();
        fs::write(&launcher, self.launcher_script(serial_port))?;
        let cmd = format!("cmd.exe /c \"{}\"", launcher.display());

        let argv: Vec<String> = ["schtasks", "/create", "/tn", TASK_NAME, "/tr", &cmd, "/sc", "onlogon", "/f"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let result = self.host.run(&argv)?;
        if result.returncode != 0 {
            self.host.eprint(&format!(
                "schtasks create failed ({}): {}",
                result.returncode,
                py_strip(&result.stderr)
            ));
            return Ok(2);
        }

        self.host.print(&format!("installed: Task Scheduler task '{TASK_NAME}'"));
        self.host.print(&format!("logs at:   {}", self.log_path.display()));
        self.host.print("daemon will start on your next login.");
        self.host.print("To start now, run: eve-bridge daemon");
        Ok(0)
    }

    /// `uninstall`: delete the task and its launcher script.
    pub fn uninstall(&self) -> anyhow::Result<i32> {
        if !self.is_installed()? {
            self.host.print("service not installed; nothing to do");
            return Ok(0);
        }
        let argv: Vec<String> =
            ["schtasks", "/delete", "/tn", TASK_NAME, "/f"].iter().map(|s| s.to_string()).collect();
        let result = self.host.run(&argv)?;
        if result.returncode != 0 {
            self.host.eprint(&format!(
                "schtasks delete failed ({}): {}",
                result.returncode,
                py_strip(&result.stderr)
            ));
            return Ok(2);
        }
        let _ = fs::remove_file(self.launcher_path());
        self.host.print(&format!("removed: Task Scheduler task '{TASK_NAME}'"));
        Ok(0)
    }

    /// `is_installed`: `schtasks /query` knows the task.
    pub fn is_installed(&self) -> io::Result<bool> {
        if self.host.which("schtasks").is_none() {
            return Ok(false);
        }
        let argv: Vec<String> = ["schtasks", "/query", "/tn", TASK_NAME].iter().map(|s| s.to_string()).collect();
        Ok(self.host.run(&argv)?.returncode == 0)
    }

    /// `is_loaded`: the same as [`TaskScheduler::is_installed`].
    pub fn is_loaded(&self) -> io::Result<bool> {
        self.is_installed()
    }

    /// `unit_path`: the task, named.
    pub fn unit_path(&self) -> String {
        format!("Task Scheduler: {TASK_NAME}")
    }

    /// `log_path`.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }
}
