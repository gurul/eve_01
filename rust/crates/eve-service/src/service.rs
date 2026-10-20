//! Daemon auto-start service: install/uninstall a user-level unit. Port of `service.py`.
//!
//! Dispatches to a platform-specific backend:
//!
//! * `darwin` → launchd user agent ([`crate::service_launchd`])
//! * `linux`  → systemd user unit  ([`crate::service_systemd`])
//! * `win32`  → Task Scheduler task ([`crate::service_windows`])
//!
//! Other platforms aren't supported; the install/uninstall calls return a non-zero exit code with a helpful
//! message instead.
//!
//! The public surface (`install_service`, `uninstall_service`, `is_installed`, `is_loaded`, `unit_path`,
//! `log_path`, `backend_name`) is platform-agnostic so callers (CLI, status output) don't need to branch on the
//! platform.

use std::fmt;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use eve_claude_home::HomeDirError;

use crate::host::{Host, SystemHost};
use crate::service_launchd::{self, Launchd};
use crate::service_systemd::{self, Systemd};
use crate::service_windows::{self, TaskScheduler};

/// Python's `sys.platform` for this build: `darwin`, `linux`, `win32`, or the OS name otherwise.
pub fn sys_platform() -> &'static str {
    match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
}

/// Which backend a `sys.platform` value gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    Launchd,
    Systemd,
    TaskScheduler,
}

impl BackendKind {
    /// The backend's `NAME`.
    pub fn name(self) -> &'static str {
        match self {
            BackendKind::Launchd => service_launchd::NAME,
            BackendKind::Systemd => service_systemd::NAME,
            BackendKind::TaskScheduler => service_windows::NAME,
        }
    }
}

/// `_backend`'s choice for `platform`.
pub fn backend_kind(platform: &str) -> Option<BackendKind> {
    if platform == "darwin" {
        Some(BackendKind::Launchd)
    } else if platform.starts_with("linux") {
        Some(BackendKind::Systemd)
    } else if platform == "win32" {
        Some(BackendKind::TaskScheduler)
    } else {
        None
    }
}

/// A backend, with its state.
#[derive(Debug, Clone)]
pub enum Backend {
    Launchd(Launchd),
    Systemd(Systemd),
    TaskScheduler(TaskScheduler),
}

impl Backend {
    /// `_backend()`: the backend for `platform`, built from this process (Python's module import, which fixes its
    /// paths from `Path.home()`). None on an unsupported platform.
    pub fn for_platform(platform: &str) -> Result<Option<Backend>, HomeDirError> {
        Ok(match backend_kind(platform) {
            Some(BackendKind::Launchd) => Some(Backend::Launchd(Launchd::from_process()?)),
            Some(BackendKind::Systemd) => Some(Backend::Systemd(Systemd::from_process()?)),
            Some(BackendKind::TaskScheduler) => Some(Backend::TaskScheduler(TaskScheduler::from_process()?)),
            None => None,
        })
    }

    /// The backend's `NAME`.
    pub fn name(&self) -> &'static str {
        match self {
            Backend::Launchd(_) => service_launchd::NAME,
            Backend::Systemd(_) => service_systemd::NAME,
            Backend::TaskScheduler(_) => service_windows::NAME,
        }
    }
}

/// Where a unit or its logs are: a file (`Path` in Python), or words (`str`: the journalctl line, the task name).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    Path(PathBuf),
    Text(String),
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Location::Path(p) => write!(f, "{}", p.display()),
            Location::Text(t) => f.write_str(t),
        }
    }
}

/// `repr()` of a str, as Python prints it.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::new();
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// `_unsupported_platform_msg`.
pub fn unsupported_platform_msg(platform: &str) -> String {
    format!(
        "eve-bridge: service install is only supported on macOS, Linux, and Windows (got {}).",
        py_repr(platform)
    )
}

/// The dispatcher: a platform, its backend (None when unsupported), and where messages go.
#[derive(Clone)]
pub struct Service {
    pub platform: String,
    pub backend: Option<Backend>,
    pub host: Arc<dyn Host>,
}

impl fmt::Debug for Service {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Service").field("platform", &self.platform).field("backend", &self.backend).finish()
    }
}

impl Service {
    /// This process's platform and backend.
    pub fn from_process() -> Result<Service, HomeDirError> {
        Service::for_platform(sys_platform())
    }

    /// `platform`'s backend, built from this process.
    pub fn for_platform(platform: &str) -> Result<Service, HomeDirError> {
        Ok(Service {
            platform: platform.to_string(),
            backend: Backend::for_platform(platform)?,
            host: Arc::new(SystemHost),
        })
    }

    /// An explicit backend (tests, or a caller that carries its own environment).
    pub fn with_backend(platform: &str, backend: Option<Backend>, host: Arc<dyn Host>) -> Service {
        Service { platform: platform.to_string(), backend, host }
    }

    /// `install_service`.
    pub fn install_service(&self, serial_port: Option<&str>) -> anyhow::Result<i32> {
        match &self.backend {
            None => {
                self.host.eprint(&unsupported_platform_msg(&self.platform));
                Ok(2)
            }
            Some(Backend::Launchd(b)) => b.install(serial_port),
            Some(Backend::Systemd(b)) => b.install(serial_port),
            Some(Backend::TaskScheduler(b)) => b.install(serial_port),
        }
    }

    /// `uninstall_service`.
    pub fn uninstall_service(&self) -> anyhow::Result<i32> {
        match &self.backend {
            None => {
                self.host.eprint(&unsupported_platform_msg(&self.platform));
                Ok(2)
            }
            Some(Backend::Launchd(b)) => b.uninstall(),
            Some(Backend::Systemd(b)) => b.uninstall(),
            Some(Backend::TaskScheduler(b)) => b.uninstall(),
        }
    }

    fn widget_is_mac_only(&self) -> i32 {
        self.host.eprint(&format!("eve-bridge: the notes widget is macOS-only (got {}).", py_repr(&self.platform)));
        2
    }

    /// `install_notes_widget`: install the desktop notes widget as its own login unit (macOS only).
    pub fn install_notes_widget(&self) -> anyhow::Result<i32> {
        match &self.backend {
            Some(Backend::Launchd(b)) => b.install_widget(),
            _ => Ok(self.widget_is_mac_only()),
        }
    }

    /// `uninstall_notes_widget`.
    pub fn uninstall_notes_widget(&self) -> anyhow::Result<i32> {
        match &self.backend {
            Some(Backend::Launchd(b)) => b.uninstall_widget(),
            _ => Ok(self.widget_is_mac_only()),
        }
    }

    /// `is_installed`.
    pub fn is_installed(&self) -> io::Result<bool> {
        match &self.backend {
            None => Ok(false),
            Some(Backend::Launchd(b)) => Ok(b.is_installed()),
            Some(Backend::Systemd(b)) => Ok(b.is_installed()),
            Some(Backend::TaskScheduler(b)) => b.is_installed(),
        }
    }

    /// `is_loaded`.
    pub fn is_loaded(&self) -> io::Result<bool> {
        match &self.backend {
            None => Ok(false),
            Some(Backend::Launchd(b)) => b.is_loaded(),
            Some(Backend::Systemd(b)) => b.is_loaded(),
            Some(Backend::TaskScheduler(b)) => b.is_loaded(),
        }
    }

    /// `backend_name`: human-readable backend identifier ("launchd", "systemd", "task-scheduler"), or None.
    pub fn backend_name(&self) -> Option<&'static str> {
        self.backend.as_ref().map(Backend::name)
    }

    /// `unit_path`: path to the installed unit file, or None on unsupported platforms.
    pub fn unit_path(&self) -> Option<Location> {
        Some(match self.backend.as_ref()? {
            Backend::Launchd(b) => Location::Path(b.unit_path().to_path_buf()),
            Backend::Systemd(b) => Location::Path(b.unit_path().to_path_buf()),
            Backend::TaskScheduler(b) => Location::Text(b.unit_path()),
        })
    }

    /// `log_path`: where to look for daemon logs. A path on macOS/Windows (a real log file) and words on Linux
    /// (the `journalctl` invocation, since journald is the log store).
    pub fn log_path(&self) -> Option<Location> {
        Some(match self.backend.as_ref()? {
            Backend::Launchd(b) => Location::Path(b.log_path().to_path_buf()),
            Backend::Systemd(b) => Location::Text(b.log_path()),
            Backend::TaskScheduler(b) => Location::Path(b.log_path().to_path_buf()),
        })
    }
}

// ---- the module-level functions, against this process (Python's `service.install_service()` and the rest) -------
//
// Each builds the backend from this process when called. An Err from building it is Python's import failing
// (`Path.home()` cannot be determined); an Err from the call is an exception the Python function lets raise.

/// `install_service(serial_port)` for this process.
pub fn install_service(serial_port: Option<&str>) -> anyhow::Result<i32> {
    Service::from_process()?.install_service(serial_port)
}

/// `uninstall_service()` for this process.
pub fn uninstall_service() -> anyhow::Result<i32> {
    Service::from_process()?.uninstall_service()
}

/// `install_notes_widget()` for this process.
pub fn install_notes_widget() -> anyhow::Result<i32> {
    Service::from_process()?.install_notes_widget()
}

/// `uninstall_notes_widget()` for this process.
pub fn uninstall_notes_widget() -> anyhow::Result<i32> {
    Service::from_process()?.uninstall_notes_widget()
}

/// `is_installed()` for this process.
pub fn is_installed() -> anyhow::Result<bool> {
    Ok(Service::from_process()?.is_installed()?)
}

/// `is_loaded()` for this process.
pub fn is_loaded() -> anyhow::Result<bool> {
    Ok(Service::from_process()?.is_loaded()?)
}

/// `unit_path()` for this process.
pub fn unit_path() -> Result<Option<Location>, HomeDirError> {
    Ok(Service::from_process()?.unit_path())
}

/// `log_path()` for this process.
pub fn log_path() -> Result<Option<Location>, HomeDirError> {
    Ok(Service::from_process()?.log_path())
}

/// `backend_name()` for this process's platform, without building a backend.
pub fn backend_name() -> Option<&'static str> {
    backend_kind(sys_platform()).map(BackendKind::name)
}
