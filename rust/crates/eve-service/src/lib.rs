//! Daemon auto-start service. Port of `service.py` and its backends `_service_launchd.py` (macOS LaunchAgents,
//! full eve's, the notes widget's and eve lite's), `_service_systemd.py` (Linux user unit) and
//! `_service_windows.py` (Task Scheduler).
//!
//! Python reads its process state at the point of use: `sys.platform`, `sys.executable`, `Path.home()`, the
//! environment, `shutil.which`, `subprocess.run` and `print`. Here each backend is a struct carrying that state
//! (`from_process()` fills it from this process), so tests run in parallel without touching the process.

pub mod host;
pub mod plistxml;
pub mod service;
pub mod service_launchd;
pub mod service_systemd;
pub mod service_windows;

pub use host::{Completed, Host, Launch, SystemHost};
pub use service::{backend_kind, backend_name, sys_platform, Backend, BackendKind, Location, Service};
pub use service_launchd::{FullPaths, Launchd};
pub use service_systemd::Systemd;
pub use service_windows::TaskScheduler;

#[cfg(test)]
mod tests;
