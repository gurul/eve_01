//! Tap one allowlisted key on the host, on request from the board.
//!
//! A swipe on the pet sends `{"cmd":"key","name":"enter"|"next"|"prev"}` and
//! the bridge synthesizes that single keystroke, so Claude Code's option
//! pickers can be answered without reaching for the keyboard.
//!
//! Hold-the-pet push-to-talk used to live here too — a finger on the pet held
//! a dictation app's global hotkey. It is gone (owner request): the board no
//! longer sends voice frames, and nothing here holds a modifier down any more.
//! What is left is the tap, which is bounded, idempotent and cannot stick a
//! key.
//!
//! Mechanics: Quartz `CGEventPost` at the HID tap. Taps use a NULL event
//! source — with a real source, Warp's global key handling swallowed Return
//! before the focused app saw it. `EVE_KEY_SOURCE=hid` forces the real source
//! if some other app ever needs the opposite, and `EVE_KEY_METHOD=osascript`
//! routes through System Events instead (Warp ignores CGEventPost synthetics).
//!
//! Requires macOS Accessibility permission for the daemon's binary —
//! CGEventPost is silently filtered for untrusted processes, so without the
//! check the feature would just mysteriously do nothing.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

pub const KEY_RETURN: u16 = 36; // kVK_Return

/// Event flag masks (Quartz constants, hardcoded so the crate builds on
/// non-mac hosts for testing). Read by listen_key, which watches for a real
/// Option press rather than synthesizing one.
pub const FLAG_ALTERNATE: u64 = 0x0008_0000; // kCGEventFlagMaskAlternate
pub const FLAG_SECONDARY_FN: u64 = 0x0080_0000; // kCGEventFlagMaskSecondaryFn

pub const KEY_KEYPAD_ENTER: u16 = 76; // kVK_ANSI_KeypadEnter
pub const KEY_DOWN_ARROW: u16 = 125; // kVK_DownArrow
pub const KEY_UP_ARROW: u16 = 126; // kVK_UpArrow

/// `EVE_ENTER_KEY` value → the keycode "enter" taps.
///
/// "enter" resolves to the main Return key (kVK_Return 36) by default. A few
/// apps distinguish it from the numeric keypad's Enter (kVK_ANSI_KeypadEnter
/// 76) — notably some editors and terminal multiplexers, where Return inserts
/// a newline and keypad Enter submits. `EVE_ENTER_KEY=keypad` switches it.
pub fn enter_keycode_for(value: Option<&str>) -> u16 {
    let raw = match value {
        Some(v) if !v.is_empty() => v,
        _ => "return",
    };
    let choice = raw.trim().to_lowercase();
    if matches!(choice.as_str(), "keypad" | "keypad-enter" | "enter") {
        return KEY_KEYPAD_ENTER;
    }
    KEY_RETURN
}

/// Python `_enter_keycode`: reads `EVE_ENTER_KEY` now.
pub fn enter_keycode() -> u16 {
    enter_keycode_for(env_var("EVE_ENTER_KEY").as_deref())
}

/// The allowlist for a given "enter" keycode.
///
/// Keys the board may ask us to tap. Deliberately a tiny allowlist: the board
/// is a peripheral on a serial line, and "synthesize any keystroke on
/// request" is a much larger surface than this feature needs.
///
/// "next"/"prev" come from horizontal swipes on the pet and navigate Claude
/// Code's option pickers, which are vertical lists — so they map to Down/Up
/// arrows. Kept semantic on the wire so the firmware never hardcodes keycodes.
pub fn tappable_with(enter: u16) -> HashMap<&'static str, u16> {
    HashMap::from([
        ("enter", enter),
        ("next", KEY_DOWN_ARROW),
        ("prev", KEY_UP_ARROW),
    ])
}

/// Python `TAPPABLE`: computed once, the first time it is read (Python
/// computes it at import), from `EVE_ENTER_KEY`.
pub static TAPPABLE: LazyLock<HashMap<&'static str, u16>> =
    LazyLock::new(|| tappable_with(enter_keycode()));

/// Poster signature: `(keycode, down)`.
pub type Poster = Box<dyn Fn(u16, bool) + Send + Sync>;

/// Accessibility trust check; the argument is `prompt` (show the system
/// dialog). A check that does not care about prompting just ignores it.
pub type TrustCheck = Box<dyn FnMut(bool) -> bool + Send>;

fn env_var(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// `EVE_KEY_SOURCE` → true when the real HID source is forced.
pub fn force_hid_for(value: Option<&str>) -> bool {
    value.unwrap_or("").trim().to_lowercase() == "hid"
}

/// The source choice of [`quartz_poster`], independent of Quartz.
///
/// Source choice is load-bearing and app-dependent:
///
/// * Plain TAPS want the NULL source. With a real source, Warp's global key
///   handling swallowed Return before it reached the focused terminal app —
///   the dictated text sat in the prompt unsent. NULL-source Return worked
///   for hours before this was changed, so taps go back to it.
///
/// `EVE_KEY_SOURCE=hid` forces the real source everywhere if some other app
/// ever needs the opposite.
pub fn poster_from<S, F>(hid_source: Option<S>, force_hid: bool, post: F) -> Poster
where
    S: Send + Sync + 'static,
    F: Fn(Option<&S>, u16, bool) + Send + Sync + 'static,
{
    Box::new(move |keycode, down| {
        let src = if force_hid { hid_source.as_ref() } else { None };
        post(src, keycode, down);
    })
}

/// Build the real CGEvent poster; `None` when Quartz is unavailable (off
/// macOS).
#[cfg(target_os = "macos")]
pub fn quartz_poster() -> Option<Poster> {
    use objc2_core_foundation::CFRetained;
    use objc2_core_graphics::{
        CGEvent, CGEventSource, CGEventSourceStateID, CGEventTapLocation,
    };

    // A real event source rather than None. Events created with a NULL source
    // carry no keyboard state and some apps drop them; HIDSystemState makes
    // the synthesized key look like it came from the actual keyboard.
    struct Src(CFRetained<CGEventSource>);
    // SAFETY: CGEventSource is a CF object; CF retain/release is thread-safe
    // and the source is only read (passed to CGEventCreateKeyboardEvent).
    unsafe impl Send for Src {}
    unsafe impl Sync for Src {}

    let src = CGEventSource::new(CGEventSourceStateID::HIDSystemState).map(Src);
    let force_hid = force_hid_for(env_var("EVE_KEY_SOURCE").as_deref());
    Some(poster_from(src, force_hid, |src: Option<&Src>, keycode, down| {
        let ev = CGEvent::new_keyboard_event(src.map(|s| &*s.0), keycode, down);
        CGEvent::post(CGEventTapLocation::HIDEventTap, ev.as_deref());
    }))
}

/// Build the real CGEvent poster; `None` when Quartz is unavailable (off
/// macOS).
#[cfg(not(target_os = "macos"))]
pub fn quartz_poster() -> Option<Poster> {
    None
}

/// True if this process may post events.
///
/// `prompt` shows the system "would like to control this computer" dialog —
/// pass it at most ONCE per daemon life. Prompting on every failed attempt
/// re-pops the dialog on every tap, which is indistinguishable from the grant
/// not working and trains the user to dismiss it.
#[cfg(target_os = "macos")]
pub fn check_accessibility(prompt: bool) -> bool {
    use objc2_application_services::{
        kAXTrustedCheckOptionPrompt, AXIsProcessTrusted, AXIsProcessTrustedWithOptions,
    };
    use objc2_core_foundation::{CFBoolean, CFDictionary, CFString};

    if prompt {
        // SAFETY: the key is a static CFString from ApplicationServices.
        let key: &CFString = unsafe { kAXTrustedCheckOptionPrompt };
        let value = CFBoolean::new(true);
        let dict = CFDictionary::<CFString, CFBoolean>::from_slices(&[key], &[value]);
        // SAFETY: a CFDictionary with CFString keys, as the API expects.
        return unsafe { AXIsProcessTrustedWithOptions(Some(dict.as_opaque())) };
    }
    // SAFETY: no arguments; reads this process's trust state.
    unsafe { AXIsProcessTrusted() }
}

/// Off macOS ApplicationServices is unavailable: can't check — post anyway.
#[cfg(not(target_os = "macos"))]
pub fn check_accessibility(_prompt: bool) -> bool {
    // Can't check — post anyway; if trust is missing the events are silently
    // dropped, and the log line below is the only breadcrumb.
    tracing::warn!(
        "key: ApplicationServices unavailable — cannot verify Accessibility permission; \
         if swipe-to-key does nothing, grant it to the daemon's python in System Settings \
         > Privacy & Security"
    );
    true
}

/// Python `repr()` of a str, for the "refusing unknown key" log line.
fn py_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
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
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `repr()` of a list of str (for the TimeoutExpired message).
fn py_list_repr(items: &[String]) -> String {
    let inner: Vec<String> = items.iter().map(|s| py_repr(s)).collect();
    format!("[{}]", inner.join(", "))
}

/// How a subprocess run ended.
#[derive(Debug)]
pub enum RunError {
    /// Did not finish in time; the child was killed.
    Timeout,
    /// Could not be started.
    Spawn(std::io::Error),
}

/// `subprocess.run(argv, capture_output=True, timeout=...)`: run, capture
/// both streams, kill on timeout.
pub fn run_with_timeout(argv: &[String], timeout: Duration) -> Result<Output, RunError> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::Spawn)?;
    let mut so = child.stdout.take();
    let mut se = child.stderr.take();
    let out_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = so.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });
    let err_t = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(s) = se.as_mut() {
            let _ = s.read_to_end(&mut buf);
        }
        buf
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RunError::Timeout);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(e) => return Err(RunError::Spawn(e)),
        }
    };
    let stdout = out_t.join().unwrap_or_default();
    let stderr = err_t.join().unwrap_or_default();
    Ok(Output { status, stdout, stderr })
}

/// The `str()` of the exception Python's `subprocess.run` raises.
fn run_error_text(argv: &[String], timeout: Duration, err: &RunError) -> String {
    match err {
        RunError::Timeout => format!(
            "Command '{}' timed out after {} seconds",
            py_list_repr(argv),
            timeout.as_secs()
        ),
        RunError::Spawn(e) => {
            let errno = e.raw_os_error().unwrap_or(0);
            if e.kind() == std::io::ErrorKind::NotFound {
                format!("[Errno {errno}] No such file or directory: {}", py_repr(&argv[0]))
            } else {
                e.to_string()
            }
        }
    }
}

/// `os.path.realpath`: resolve symlinks, and fall back to the path itself
/// when it cannot be resolved (realpath never raises).
fn realpath(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Python's `str.strip()` then `[:200]` (characters, not bytes).
fn strip_200(s: &str) -> String {
    s.trim().chars().take(200).collect()
}

/// Python `KeyTapper.diagnose`'s code-signature probe on `codesign -dv`
/// output: `"ad-hoc / linker-signed"` or `"signed"`.
pub fn signature_kind(stdout: &str, stderr: &str) -> &'static str {
    let blob = if stderr.is_empty() { stdout } else { stderr };
    let adhoc = blob.contains("adhoc") || blob.contains("Signature=adhoc");
    if adhoc {
        "ad-hoc / linker-signed"
    } else {
        "signed"
    }
}

/// Synthesizes single keystrokes the board asks for. Stateless per tap.
pub struct KeyTapper {
    poster: Option<Poster>,
    trust_check: Option<TrustCheck>,
    prompted: bool, // the system dialog is shown at most once
    keys: HashMap<&'static str, u16>,
}

impl Default for KeyTapper {
    fn default() -> Self {
        Self::new(None, None)
    }
}

impl KeyTapper {
    /// `KeyTapper(poster=None, trust_check=None)`.
    ///
    /// An injected poster (tests) skips the Accessibility machinery entirely;
    /// the real Quartz poster gets the real trust check unless overridden.
    pub fn new(poster: Option<Poster>, trust_check: Option<TrustCheck>) -> Self {
        Self::with_factory(poster, trust_check, quartz_poster)
    }

    /// [`KeyTapper::new`] with the poster factory as a seam (Python tests
    /// monkeypatch `_quartz_poster`).
    pub fn with_factory(
        poster: Option<Poster>,
        mut trust_check: Option<TrustCheck>,
        factory: impl FnOnce() -> Option<Poster>,
    ) -> Self {
        let poster = match poster {
            Some(p) => Some(p),
            None => {
                let p = factory();
                if trust_check.is_none() && p.is_some() {
                    trust_check = Some(Box::new(check_accessibility));
                }
                p
            }
        };
        Self {
            poster,
            trust_check,
            prompted: false,
            keys: TAPPABLE.clone(),
        }
    }

    /// Replace the allowlist (Python reads the module-level `TAPPABLE`; this
    /// lets a caller pin it without touching the process environment).
    pub fn with_keys(mut self, keys: HashMap<&'static str, u16>) -> Self {
        self.keys = keys;
        self
    }

    /// Print why swipe-to-key is or isn't working. Returns an exit code.
    ///
    /// Accessibility is granted per *binary*, and a venv's python is a symlink
    /// — macOS resolves it, so the path that must appear (and be toggled ON)
    /// in System Settings is the real binary, not the symlinked one.
    pub fn diagnose(&self) -> i32 {
        // Python prints `sys.executable` (the interpreter as launched, often
        // a venv symlink); the Rust daemon's binary as launched is
        // `current_exe` (not symlink-resolved on macOS).
        let exe: PathBuf = std::env::current_exe().unwrap_or_default();
        let real = realpath(&exe);
        println!("daemon python (argv):  {}", exe.display());
        println!("resolved binary:       {}", real.display());
        if real != exe {
            println!("  ^ ADD THIS ONE in System Settings — the venv path is a symlink");
        }

        let mut sig = "unknown";
        let argv = vec![
            "codesign".to_string(),
            "-dv".to_string(),
            "--verbose=2".to_string(),
            real.to_string_lossy().into_owned(),
        ];
        if let Ok(out) = run_with_timeout(&argv, Duration::from_secs(10)) {
            sig = signature_kind(
                &String::from_utf8_lossy(&out.stdout),
                &String::from_utf8_lossy(&out.stderr),
            );
        }
        println!("code signature:        {sig}");
        if sig.starts_with("ad-hoc") {
            println!("  note: ad-hoc-signed interpreters (uv/pyenv builds) are the");
            println!("  usual cause of a grant that 'won't stick' — macOS keys the");
            println!("  grant to the signature. Remove every stale python entry in");
            println!("  the Accessibility list, then re-add the resolved path above.");
        }

        if self.poster.is_none() {
            println!("quartz poster:         UNAVAILABLE (pyobjc not installed?)");
            return 2;
        }
        println!("quartz poster:         ok");

        let trusted = check_accessibility(false);
        println!(
            "accessibility trusted: {}",
            if trusted { "True" } else { "False" }
        );
        if !trusted {
            println!("\nFIX: System Settings > Privacy & Security > Accessibility");
            println!("  1. remove any existing 'python3.12' rows (stale grants)");
            println!("  2. '+', then Cmd+Shift+G, paste: {}", real.display());
            println!("  3. make sure its toggle is ON (adding alone does not enable it)");
            println!("  4. restart the daemon:");
            println!("     launchctl kickstart -k gui/$(id -u)/com.github.eve-bridge.daemon");
            return 1;
        }
        println!("\nReady — swipe the pet to send Enter.");
        0
    }

    /// Deliver a key through System Events instead of CGEventPost.
    ///
    /// Warp ignores CGEventPost synthetics — the Enter arrives, the log says
    /// it was tapped, and nothing submits, while the identical event works in
    /// every other app. System Events posts through a different path that
    /// Warp does accept. Costs a subprocess (~40ms), so it is opt-in.
    pub fn tap_via_system_events(keycode: u16) -> bool {
        let argv = vec![
            "osascript".to_string(),
            "-e".to_string(),
            format!("tell application \"System Events\" to key code {keycode}"),
        ];
        let timeout = Duration::from_secs(5);
        match run_with_timeout(&argv, timeout) {
            Ok(r) => {
                if !r.status.success() {
                    tracing::warn!(
                        "key: System Events failed: {}",
                        strip_200(&String::from_utf8_lossy(&r.stderr))
                    );
                    return false;
                }
                true
            }
            Err(e) => {
                tracing::warn!(
                    "key: System Events error: {}",
                    run_error_text(&argv, timeout, &e)
                );
                false
            }
        }
    }

    /// Press and release one allowlisted key (swipe-down → Enter). Reads
    /// `EVE_KEY_METHOD` on every tap.
    pub fn tap(&mut self, name: &str) -> bool {
        let method = env_var("EVE_KEY_METHOD");
        self.tap_with_method(name, method.as_deref())
    }

    /// [`KeyTapper::tap`] with the `EVE_KEY_METHOD` value passed in.
    pub fn tap_with_method(&mut self, name: &str, method: Option<&str>) -> bool {
        let Some(&key) = self.keys.get(name) else {
            tracing::warn!("key: refusing unknown key {}", py_repr(name));
            return false;
        };
        let Some(poster) = self.poster.as_ref() else {
            return false;
        };
        if let Some(check) = self.trust_check.as_mut() {
            let ok = check(!self.prompted);
            self.prompted = true;
            if !ok {
                tracing::warn!("key: Accessibility not granted — see `key-check`");
                return false;
            }
            self.trust_check = None;
        }
        let method = match method {
            Some(m) if !m.is_empty() => m,
            _ => "auto",
        }
        .trim()
        .to_lowercase();
        if matches!(method.as_str(), "osascript" | "system-events") {
            let ok = Self::tap_via_system_events(key);
            tracing::info!(
                "key: tapped {} via System Events ({})",
                name,
                if ok { "ok" } else { "FAILED" }
            );
            return ok;
        }

        poster(key, true);
        // Hold briefly. A down/up in the same microsecond is not a keypress
        // any human could produce, and apps that debounce or sample input on
        // a frame boundary drop it entirely.
        std::thread::sleep(Duration::from_millis(30));
        poster(key, false);
        tracing::info!("key: tapped {}", name);
        true
    }
}

#[cfg(test)]
mod tests;
