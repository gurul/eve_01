//! `eve-desktop-worker`: the daemon's desktop worker child (Python `python -m eve_bridge.desktop_worker`).
//!
//! `--release` posts a key-up for every key code and a mouse-up for every button, then exits 0. `--check` stops
//! after the ready line. Any failure before or while serving is one `{"error": {"code": "startup", …}}` line and
//! exit 1, as the Python's `main()` wrapper did; an exec host that dies after the ready line ends the worker
//! without a line, as the Python worker's own death did.

use eve_desktop_lanes::{error_kind, error_message, py_take};
use eve_desktop_worker::{emit, main_with, HostDied};
use serde_json::json;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--release") {
        #[cfg(target_os = "macos")]
        {
            eve_desktop_worker::release_inputs(&mut eve_desktop_worker::SystemQuartz);
            std::process::exit(0);
        }
        #[cfg(not(target_os = "macos"))]
        {
            eprintln!("ModuleNotFoundError: No module named 'Quartz'");
            std::process::exit(1);
        }
    }
    let check = args.iter().any(|a| a == "--check");
    if let Err(error) = main_with(check) {
        if error.downcast_ref::<HostDied>().is_some() && eve_desktop_worker::ready_sent() {
            eprintln!("desktop worker: {error}");
            std::process::exit(1);
        }
        let message = py_take(&format!("{}: {}", error_kind(&error), error_message(&error)), 4000);
        let _ = emit(&json!({"error": {"code": "startup", "message": message}}));
        std::process::exit(1);
    }
}
