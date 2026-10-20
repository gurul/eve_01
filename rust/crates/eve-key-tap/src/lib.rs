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
use std::sync::{mpsc, LazyLock};
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

/// Python `str.isprintable()` for one character, from the categories CPython
/// treats as non-printable: Cc, Cf, Cs, Co, Zl, Zp, Zs other than the space,
/// and the noncharacters. Unassigned (Cn) code points outside those ranges
/// are treated as printable (that needs the full Unicode tables).
fn py_printable(c: char) -> bool {
    let u = c as u32;
    if u == 0x20 {
        return true;
    }
    let not = matches!(u,
        // Cc
        0x00..=0x1f | 0x7f..=0x9f
        // Zs (other than space), Zl, Zp
        | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000
        // Cf
        | 0xad | 0x600..=0x605 | 0x61c | 0x6dd | 0x70f | 0x890..=0x891 | 0x8e2 | 0x180e
        | 0x200b..=0x200f | 0x202a..=0x202e | 0x2060..=0x2064 | 0x2066..=0x206f | 0xfeff
        | 0xfff9..=0xfffb | 0x110bd | 0x110cd | 0x13430..=0x1343f | 0x1bca0..=0x1bca3
        | 0x1d173..=0x1d17a | 0xe0001 | 0xe0020..=0xe007f
        // Co
        | 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd
        // noncharacters (Cn)
        | 0xfdd0..=0xfdef
    );
    !(not || (u & 0xfffe) == 0xfffe)
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
            c if !py_printable(c) => {
                let u = c as u32;
                if u < 0x100 {
                    out.push_str(&format!("\\x{u:02x}"));
                } else if u < 0x10000 {
                    out.push_str(&format!("\\u{u:04x}"));
                } else {
                    out.push_str(&format!("\\U{u:08x}"));
                }
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
/// both streams, kill on timeout. Like Python's `communicate`, the deadline
/// covers both the exit and the pipes closing.
pub fn run_with_timeout(argv: &[String], timeout: Duration) -> Result<Output, RunError> {
    let mut child = Command::new(&argv[0])
        .args(&argv[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(RunError::Spawn)?;
    let (tx, rx) = mpsc::channel::<(bool, Vec<u8>)>();
    let readers: [(bool, Option<Box<dyn Read + Send>>); 2] = [
        (true, child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>)),
        (false, child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)),
    ];
    for (is_out, stream) in readers {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut s) = stream {
                let _ = s.read_to_end(&mut buf);
            }
            let _ = tx.send((is_out, buf));
        });
    }
    drop(tx);
    let deadline = Instant::now() + timeout;
    let (mut stdout, mut stderr, mut status) = (None, None, None);
    loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(Some(st)) => status = Some(st),
                Ok(None) => {}
                Err(e) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(RunError::Spawn(e));
                }
            }
        }
        if let (Some(st), Some(_), Some(_)) = (status, stdout.as_ref(), stderr.as_ref()) {
            return Ok(Output {
                status: st,
                stdout: stdout.unwrap_or_default(),
                stderr: stderr.unwrap_or_default(),
            });
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(RunError::Timeout);
        }
        match rx.recv_timeout(Duration::from_millis(5)) {
            Ok((true, buf)) => stdout = Some(buf),
            Ok((false, buf)) => stderr = Some(buf),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // Both readers reported; only the exit is left to wait on.
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

/// The `str()` of the exception Python's `subprocess.run` raises.
fn run_error_text(argv: &[String], timeout: Duration, err: &RunError) -> String {
    match err {
        RunError::Timeout => format!(
            "Command '{}' timed out after {} seconds",
            py_list_repr(argv),
            timeout.as_secs()
        ),
        RunError::Spawn(e) => match e.raw_os_error() {
            // OSError(errno, strerror, filename): "[Errno 2] No such file or
            // directory: 'osascript'". Rust's Display is
            // "<strerror> (os error N)"; the strerror part is the same text.
            Some(errno) => {
                let text = e.to_string();
                let suffix = format!(" (os error {errno})");
                let strerror = text.strip_suffix(&suffix).unwrap_or(&text);
                format!("[Errno {errno}] {strerror}: {}", py_repr(&argv[0]))
            }
            None => e.to_string(),
        },
    }
}

/// `subprocess`'s text mode: strict UTF-8 decode plus universal newlines.
/// The error is the `str()` of Python's `UnicodeDecodeError`.
fn py_text(b: &[u8]) -> Result<String, String> {
    match std::str::from_utf8(b) {
        Ok(s) => Ok(s.replace("\r\n", "\n").replace('\r', "\n")),
        Err(e) => {
            let start = e.valid_up_to();
            let lead = b[start];
            let (end, reason) = match e.error_len() {
                None => (b.len(), "unexpected end of data"),
                Some(n) if (0xc2..=0xf4).contains(&lead) => {
                    (start + n, "invalid continuation byte")
                }
                Some(_) => (start + 1, "invalid start byte"),
            };
            Err(if end - start == 1 {
                format!("'utf-8' codec can't decode byte 0x{lead:02x} in position {start}: {reason}")
            } else {
                format!(
                    "'utf-8' codec can't decode bytes in position {start}-{}: {reason}",
                    end - 1
                )
            })
        }
    }
}

/// Decode a finished run's stdout then stderr, as `text=True` does.
fn py_text_output(out: &Output) -> Result<(String, String), String> {
    let stdout = py_text(&out.stdout)?;
    let stderr = py_text(&out.stderr)?;
    Ok((stdout, stderr))
}

/// `os.path.realpath`: resolve symlinks, and fall back to the path itself
/// when it cannot be resolved (realpath never raises).
fn realpath(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Python's `str.strip()` then `[:200]` (characters, not bytes). Python's
/// whitespace is Unicode White_Space plus the separators U+001C..U+001F.
fn strip_200(s: &str) -> String {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
        .chars()
        .take(200)
        .collect()
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
        // Any failure (missing codesign, timeout, undecodable output) leaves
        // "unknown", as Python's bare except does.
        if let Ok(out) = run_with_timeout(&argv, Duration::from_secs(10)) {
            if let Ok((stdout, stderr)) = py_text_output(&out) {
                sig = signature_kind(&stdout, &stderr);
            }
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
            Ok(r) => match py_text_output(&r) {
                Ok((_, stderr)) => {
                    if !r.status.success() {
                        tracing::warn!("key: System Events failed: {}", strip_200(&stderr));
                        return false;
                    }
                    true
                }
                // text=True raised UnicodeDecodeError inside subprocess.run.
                Err(e) => {
                    tracing::warn!("key: System Events error: {}", e);
                    false
                }
            },
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
