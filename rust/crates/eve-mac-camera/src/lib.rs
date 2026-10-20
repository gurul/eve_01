//! The Mac's own camera: one still, on request. Port of `eve_bridge/mac_camera.py`.
//!
//! take_photo was the robot's camera (daemon._photo_for_owner snaps the board). The robot body was retired on
//! 2026-10-03, and from then "take a photo from my Mac and send it to me" had no tool: the brain handed it to
//! start_task, Codex's Computer Use took the picture in about 70 s, and it reached the chat as a file, not a photo
//! (owner, 2026-10-06). This is the tool's other camera: ffmpeg reads one frame from AVFoundation in about a second.
//!
//! The camera is `EVE_MAC_CAMERA`: an AVFoundation device name or index (`ffmpeg -f avfoundation
//! -list_devices true -i ""` lists them), or `default`, the Mac's default camera, when unset. `off` turns the
//! Mac camera off. The first frames after a webcam opens are dark while its exposure settles, so the still is frame
//! [`WARMUP_FRAMES`] (half a second at 30 fps; measured 2026-10-06 on an OBSBOT Tiny 3: frame 5 had mean luma 134,
//! frame 15 had 189). It is scaled to at most [`MAX_WIDTH`] wide: a 4K webcam's still is more than a chat needs.
//!
//! macOS asks for camera access once, for the process that runs ffmpeg (the daemon). A refusal, a missing ffmpeg
//! or a camera in use by another app comes back as a reason, never an error.

use std::collections::HashMap;
use std::ffi::CString;
use std::process::Stdio;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::process::Command;

pub const ENV: &str = "EVE_MAC_CAMERA";
pub const DEFAULT_DEVICE: &str = "default";
pub const WARMUP_FRAMES: u32 = 15;
pub const FRAMERATE: u32 = 30;
pub const MAX_WIDTH: u32 = 1920;
pub const TIMEOUT_SECS: f64 = 12.0;
/// launchd's PATH has neither.
pub const FFMPEG_PATHS: [&str; 2] = ["/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg"];

pub const REASON_OFF: &str = "the Mac's camera is switched off for eve";
pub const REASON_NO_FFMPEG: &str = "ffmpeg is not installed, so eve cannot read the Mac's camera";
pub const REASON_NOT_STARTED: &str = "the Mac's camera could not be started";
pub const REASON_TIMEOUT: &str = "the Mac's camera did not answer in time";
pub const REASON_NO_PICTURE: &str =
    "the Mac's camera gave no picture (is another app using it, or is camera access off for eve?)";

/// Python's `{"ok": True, "jpeg": bytes}` or `{"ok": False, "reason": ...}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Snap {
    Ok { jpeg: Vec<u8> },
    Failed { reason: String },
}

impl Snap {
    /// The dict's `"ok"`.
    pub fn ok(&self) -> bool {
        matches!(self, Snap::Ok { .. })
    }

    /// The dict's `"jpeg"`, when there is one.
    pub fn jpeg(&self) -> Option<&[u8]> {
        match self {
            Snap::Ok { jpeg } => Some(jpeg),
            Snap::Failed { .. } => None,
        }
    }

    /// The dict's `"reason"`, when there is one.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Snap::Ok { .. } => None,
            Snap::Failed { reason } => Some(reason),
        }
    }

    fn failed(reason: &str) -> Self {
        Snap::Failed { reason: reason.to_string() }
    }
}

/// The AVFoundation device to read, or `None` when the Mac camera is off.
///
/// `environ` is the mapping to read; `None` reads the process environment, as Python's `environ=None` does.
pub fn device(environ: Option<&HashMap<String, String>>) -> Option<String> {
    let raw = match environ {
        Some(env) => env.get(ENV).cloned().unwrap_or_default(),
        None => std::env::var_os(ENV).map(|v| v.to_string_lossy().into_owned()).unwrap_or_default(),
    };
    let raw = raw.trim();
    if matches!(raw.to_lowercase().as_str(), "off" | "0" | "false" | "no" | "none") {
        return None;
    }
    Some(if raw.is_empty() { DEFAULT_DEVICE.to_string() } else { raw.to_string() })
}

fn executable(path: &str) -> bool {
    // os.access(p, os.X_OK)
    match CString::new(path) {
        // SAFETY: `c` is a valid NUL-terminated string that outlives the call.
        Ok(c) => unsafe { libc::access(c.as_ptr(), libc::X_OK) == 0 },
        Err(_) => false,
    }
}

/// `shutil.which("ffmpeg")`, else the first executable of [`FFMPEG_PATHS`].
pub fn ffmpeg_path() -> Option<String> {
    if let Ok(found) = which::which("ffmpeg") {
        return Some(found.to_string_lossy().into_owned());
    }
    FFMPEG_PATHS.iter().find(|p| executable(p)).map(|p| p.to_string())
}

/// One JPEG of frame [`WARMUP_FRAMES`] on stdout. `<dev>:none` opens the camera without a microphone.
pub fn command(ffmpeg: &str, dev: &str) -> Vec<String> {
    [
        ffmpeg,
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-f",
        "avfoundation",
        "-pixel_format",
        "nv12",
        "-framerate",
        &FRAMERATE.to_string(),
        "-i",
        &format!("{dev}:none"),
        "-vf",
        &format!("select=gte(n\\,{WARMUP_FRAMES}),scale='min({MAX_WIDTH},iw)':-2"),
        "-frames:v",
        "1",
        "-q:v",
        "3",
        "-f",
        "image2pipe",
        "-vcodec",
        "mjpeg",
        "pipe:1",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// One still from the Mac's camera: [`Snap::Ok`] with the JPEG, or [`Snap::Failed`] with a reason the owner
/// can read. Python's default timeout is [`TIMEOUT_SECS`]; callers pass it explicitly.
pub async fn snap(environ: Option<&HashMap<String, String>>, timeout: f64) -> Snap {
    snap_with(environ, timeout, ffmpeg_path).await
}

/// [`snap`] with the ffmpeg lookup as a seam (tests hand it a fake ffmpeg). It is consulted only once the
/// camera is known to be on, so a switched-off camera never looks for, nor starts, ffmpeg.
pub async fn snap_with(
    environ: Option<&HashMap<String, String>>,
    timeout: f64,
    find_ffmpeg: impl FnOnce() -> Option<String>,
) -> Snap {
    let Some(dev) = device(environ) else {
        return Snap::failed(REASON_OFF);
    };
    let Some(ffmpeg) = find_ffmpeg() else {
        return Snap::failed(REASON_NO_FFMPEG);
    };
    let argv = command(&ffmpeg, &dev);
    let mut child = match Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("mac camera: cannot start ffmpeg: {}", e);
            return Snap::failed(REASON_NOT_STARTED);
        }
    };
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    // asyncio.wait_for(proc.communicate(), timeout): a non-positive or NaN timeout times out at once.
    let limit = Duration::try_from_secs_f64(timeout).unwrap_or(Duration::ZERO);
    let communicated = tokio::time::timeout(limit, async {
        let mut out = Vec::new();
        let mut err = Vec::new();
        let read_out = async {
            if let Some(s) = stdout.as_mut() {
                let _ = s.read_to_end(&mut out).await;
            }
        };
        let read_err = async {
            if let Some(s) = stderr.as_mut() {
                let _ = s.read_to_end(&mut err).await;
            }
        };
        let ((), (), status) = tokio::join!(read_out, read_err, child.wait());
        (out, err, status)
    })
    .await;
    let (out, err, status) = match communicated {
        Ok(v) => v,
        Err(_) => {
            // proc.kill(); await proc.wait()
            let _ = child.kill().await;
            return Snap::failed(REASON_TIMEOUT);
        }
    };
    let returncode = match status {
        Ok(s) => returncode(s),
        Err(e) => {
            tracing::warn!("mac camera: cannot start ffmpeg: {}", e);
            return Snap::failed(REASON_NOT_STARTED);
        }
    };
    if returncode != Some(0) || !out.starts_with(b"\xff\xd8") {
        let tail = stderr_tail(&err);
        let shown: String = tail.chars().take(200).collect();
        let code = returncode.map_or_else(|| "None".to_string(), |c| c.to_string());
        tracing::warn!("mac camera: no picture (exit {}): {}", code, shown);
        return Snap::failed(REASON_NO_PICTURE);
    }
    Snap::Ok { jpeg: out }
}

/// Python's `Popen.returncode`: the exit code, or minus the signal that ended the process.
fn returncode(status: std::process::ExitStatus) -> Option<i32> {
    if let Some(code) = status.code() {
        return Some(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return Some(-sig);
        }
    }
    None
}

/// `(err or b"").decode("utf-8", "replace").strip().splitlines()[-1:] or [""]`, first element.
fn stderr_tail(err: &[u8]) -> String {
    let text = String::from_utf8_lossy(err);
    py_splitlines(text.trim()).pop().unwrap_or_default()
}

/// `str.splitlines()`: every line boundary Python knows, no trailing empty line.
fn py_splitlines(s: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut cur = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                lines.push(std::mem::take(&mut cur));
            }
            '\n' | '\u{0b}' | '\u{0c}' | '\u{1c}' | '\u{1d}' | '\u{1e}' | '\u{85}' | '\u{2028}' | '\u{2029}' => {
                lines.push(std::mem::take(&mut cur));
            }
            _ => cur.push(c),
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

#[cfg(test)]
mod tests;
