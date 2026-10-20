//! Accessibility and Screen Recording as macOS sees this process.

use serde_json::{json, Value};

/// Accessibility and Screen Recording as macOS sees THIS process.
///
/// TCC credits the responsible process: run from a terminal that is a terminal's grant, under launchd it is the
/// binary's. `"python"` keeps its Python key name and holds this executable's real path (Python wrote
/// `os.path.realpath(sys.executable)`, the binary TCC credits). `accessibility` and `screen` are null off macOS.
pub fn desktop_grants(prompt: bool) -> Value {
    let exe = std::env::current_exe()
        .ok()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_default();
    #[allow(unused_mut)]
    let mut out = json!({"python": exe, "accessibility": null, "screen": null});
    #[cfg(target_os = "macos")]
    {
        out["accessibility"] = json!(mac::ax_trusted());
        let ok = mac::screen_preflight();
        if !ok && prompt {
            // shows the system dialog, once per binary
            mac::screen_request();
        }
        out["screen"] = json!(ok);
    }
    #[cfg(not(target_os = "macos"))]
    let _ = prompt;
    out
}

#[cfg(target_os = "macos")]
mod mac {
    #[link(name = "ApplicationServices", kind = "framework")]
    extern "C" {
        fn AXIsProcessTrusted() -> u8;
    }

    #[link(name = "CoreGraphics", kind = "framework")]
    extern "C" {
        fn CGPreflightScreenCaptureAccess() -> bool;
        fn CGRequestScreenCaptureAccess() -> bool;
    }

    pub fn ax_trusted() -> bool {
        // SAFETY: a plain C call with no arguments; it reads the TCC database for this process.
        unsafe { AXIsProcessTrusted() != 0 }
    }

    pub fn screen_preflight() -> bool {
        // SAFETY: as above.
        unsafe { CGPreflightScreenCaptureAccess() }
    }

    pub fn screen_request() -> bool {
        // SAFETY: as above; it may show the system's permission dialog.
        unsafe { CGRequestScreenCaptureAccess() }
    }
}
