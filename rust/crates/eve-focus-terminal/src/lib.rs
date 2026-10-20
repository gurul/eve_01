//! Raise the terminal window running a given session.
//!
//! One gesture lands here. Tap the pet while it demands attention = "take me
//! there": a session is blocked on input and the human wants the terminal in
//! front. We (usually) know the session's cwd; the best cross-terminal
//! heuristic is a window whose title mentions the cwd basename — Terminal.app
//! titles include the working directory by default, and iTerm2 exposes
//! per-session names the same way.
//!
//! Strategy, in order:
//!   1. iTerm2 running → AppleScript search its windows/tabs/sessions for the
//!      repo name; select + raise on match.
//!   2. Terminal.app running → same over its windows/tabs.
//!   3. Match failed or errored → just activate the frontmost of the two apps,
//!      which at least lands the human in their terminal.
//!
//! Requires macOS Automation permission (daemon → the terminal app); macOS
//! prompts once on first use. Failure is always non-fatal — worst case the
//! card still works and nothing raises.

use std::process::Stdio;

use async_trait::async_trait;
use tokio::process::Command;

pub const ITERM_SCRIPT: &str = r#"
on run argv
  set needle to item 1 of argv
  tell application "iTerm2"
    repeat with w in windows
      repeat with t in tabs of w
        repeat with s in sessions of t
          if (name of s as string) contains needle then
            select w
            select t
            select s
            activate
            return "matched"
          end if
        end repeat
      end repeat
    end repeat
    activate
    return "activated"
  end tell
end run
"#;

/// Ghostty, Warp, cmux and friends expose no useful AppleScript window model
/// (Warp's is minimal; Ghostty has none), so there is no way to pick the right
/// window by cwd. Activating the app is the honest best effort: it puts the
/// human in the right program, one Cmd+` from the right window.
pub const ACTIVATE_SCRIPT: &str = r#"
on run argv
  tell application id (item 1 of argv) to activate
  return "activated"
end run
"#;

/// Same, but by app name — for apps configured via EVE_FOCUS_APPS (or
/// defaults whose bundle id isn't stable) where all we have is the name.
pub const ACTIVATE_BY_NAME_SCRIPT: &str = r#"
on run argv
  tell application (item 1 of argv) to activate
  return "activated"
end run
"#;

/// One activate candidate: (process name for the running check, bundle id or
/// None). None = activate by process name.
pub type ActivateTarget = (String, Option<String>);

/// (process name for the running check, bundle id or None) tried in order
/// after the AppleScript-capable terminals. None = activate by process name.
/// cmux id from manaflow-ai/cmux's own scripts (stable app; DEV builds append
/// .debug). Composer has no bundle id we could verify — name activation only.
pub const DEFAULT_ACTIVATE_TARGETS: &[(&str, Option<&str>)] = &[
    ("ghostty", Some("com.mitchellh.ghostty")),
    ("stable", Some("dev.warp.Warp-Stable")),
    ("Warp", Some("dev.warp.Warp")),
    ("cmux", Some("com.cmuxterm.app")),
    ("Composer", None),
    ("Cursor", Some("com.todesktop.230313mzl4w4u92")),
    ("Code", Some("com.microsoft.VSCode")),
];

/// `DEFAULT_ACTIVATE_TARGETS` as owned values.
pub fn default_activate_targets() -> Vec<ActivateTarget> {
    DEFAULT_ACTIVATE_TARGETS
        .iter()
        .map(|(n, b)| (n.to_string(), b.map(str::to_string)))
        .collect()
}

/// The ordered activate list, overridable via EVE_FOCUS_APPS.
///
/// The env var is a comma-separated list of app/process names in priority
/// order (e.g. "Warp,cmux,Composer"). A name matching a default entry keeps
/// its known bundle id; anything else activates by name.
pub fn activate_targets() -> Vec<ActivateTarget> {
    // Python reads the variable whatever its bytes; a non-UTF-8 value is
    // decoded lossily here rather than treated as unset.
    let raw = std::env::var_os("EVE_FOCUS_APPS")
        .map(|v| v.to_string_lossy().into_owned())
        .unwrap_or_default();
    activate_targets_from(&raw)
}

/// `activate_targets` over an explicit EVE_FOCUS_APPS value ("" = unset).
pub fn activate_targets_from(raw: &str) -> Vec<ActivateTarget> {
    let raw = py_strip(raw);
    if raw.is_empty() {
        return default_activate_targets();
    }
    let mut targets: Vec<ActivateTarget> = Vec::new();
    for part in raw.split(',') {
        let name = py_strip(part);
        if name.is_empty() {
            continue;
        }
        // Python builds a dict keyed by lowercased name; with duplicates the
        // last entry would win, but the defaults have no duplicate keys.
        let known = DEFAULT_ACTIVATE_TARGETS
            .iter()
            .rev()
            .find(|(n, _)| n.to_lowercase() == name.to_lowercase());
        match known {
            Some((n, b)) => targets.push((n.to_string(), b.map(str::to_string))),
            None => targets.push((name.to_string(), None)),
        }
    }
    if targets.is_empty() {
        default_activate_targets()
    } else {
        targets
    }
}

pub const TERMINAL_SCRIPT: &str = r#"
on run argv
  set needle to item 1 of argv
  tell application "Terminal"
    repeat with w in windows
      repeat with t in tabs of w
        if (custom title of t as string) contains needle or (name of w as string) contains needle then
          set selected of t to true
          set index of w to 1
          activate
          return "matched"
        end if
      end repeat
    end repeat
    activate
    return "activated"
  end tell
end run
"#;

/// Window-level matching for apps WITHOUT an AppleScript model (Warp, Ghostty,
/// cmux, …): System Events reads window titles through Accessibility — which
/// the daemon already holds for push-to-talk — and AXRaise brings the matching
/// window forward. Reports "frontmost" when the app is already up front with no
/// matching title, so the caller can say "you're already looking at it" instead
/// of silently no-op'ing an activate (the 2026-08-07 tap-test confusion).
pub const AX_RAISE_SCRIPT: &str = r#"
on run argv
  set procName to item 1 of argv
  set needle to item 2 of argv
  tell application "System Events"
    if not (exists process procName) then return "no-proc"
    tell process procName
      repeat with w in windows
        if (name of w as string) contains needle then
          try
            if value of attribute "AXMinimized" of w is true then
              set value of attribute "AXMinimized" of w to false
            end if
          end try
          perform action "AXRaise" of w
          set frontmost to true
          return "raised"
        end if
      end repeat
      if frontmost then return "frontmost"
    end tell
  end tell
  return "no-match"
end run
"#;

/// `activate` does NOT pull a minimized window out of the Dock — an app whose
/// only window is minimized comes "frontmost" showing nothing, which reads as
/// the button doing nothing (2026-08-11, MENU on the e-ink board). If no
/// window of the app is visible, restore the first minimized one.
pub const UNMINIMIZE_SCRIPT: &str = r#"
on run argv
  set procName to item 1 of argv
  tell application "System Events"
    if not (exists process procName) then return "no-proc"
    tell process procName
      repeat with w in windows
        try
          if value of attribute "AXMinimized" of w is false then return "visible"
        end try
      end repeat
      repeat with w in windows
        try
          if value of attribute "AXMinimized" of w is true then
            set value of attribute "AXMinimized" of w to false
            perform action "AXRaise" of w
            return "unminimized"
          end if
        end try
      end repeat
      return "no-windows"
    end tell
  end tell
end run
"#;

/// The two seams the module talks to the system through: `pgrep -x` and
/// `osascript`. An `Err` is a failure to run the tool at all (Python raises,
/// and `focus_session_terminal` logs it as a non-fatal warning).
#[async_trait]
pub trait FocusBackend: Send + Sync {
    /// Python `_app_running`: `pgrep -x <name>` exits 0.
    async fn app_running(&self, name: &str) -> anyhow::Result<bool>;
    /// Python `_osascript`: stripped stdout, or `None` when osascript exits
    /// non-zero.
    async fn osascript(&self, script: &str, args: &[&str]) -> anyhow::Result<Option<String>>;
}

/// The real backend: runs `pgrep` and `osascript` as subprocesses.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemBackend;

#[async_trait]
impl FocusBackend for SystemBackend {
    async fn app_running(&self, name: &str) -> anyhow::Result<bool> {
        app_running(name).await
    }

    async fn osascript(&self, script: &str, args: &[&str]) -> anyhow::Result<Option<String>> {
        osascript(script, args).await
    }
}

/// `pgrep -x <bundle_hint>`; true when it exits 0.
pub async fn app_running(bundle_hint: &str) -> anyhow::Result<bool> {
    let status = Command::new("pgrep")
        .arg("-x")
        .arg(bundle_hint)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await
        .map_err(|e| spawn_error(e, "pgrep"))?;
    Ok(status.code() == Some(0))
}

/// A spawn failure worded as Python's `OSError` str, e.g.
/// `[Errno 2] No such file or directory: 'osascript'`, so the
/// `focus: failed non-fatally: …` warning reads the same.
fn spawn_error(e: std::io::Error, program: &str) -> anyhow::Error {
    match e.raw_os_error() {
        Some(code) => {
            let full = e.to_string();
            let suffix = format!(" (os error {code})");
            let msg = full.strip_suffix(&suffix).unwrap_or(&full);
            anyhow::anyhow!("[Errno {code}] {msg}: {}", py_repr(program))
        }
        None => anyhow::Error::new(e),
    }
}

/// Python `str.strip()` with no arguments: strips every char whose
/// `isspace()` is true, which adds U+001C..U+001F to Rust's White_Space set.
pub fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// `osascript -e <script> <args…>`; stripped stdout, or `None` on a
/// non-zero exit (logged at debug with the first 200 chars of stderr).
pub async fn osascript(script: &str, args: &[&str]) -> anyhow::Result<Option<String>> {
    let out = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| spawn_error(e, "osascript"))?;
    if out.status.code() != Some(0) {
        let err = String::from_utf8_lossy(&out.stderr);
        let err: String = py_strip(&err).chars().take(200).collect();
        tracing::debug!("focus: osascript failed: {}", err);
        return Ok(None);
    }
    Ok(Some(py_strip(&String::from_utf8_lossy(&out.stdout)).to_string()))
}

/// Python truthiness of an `_osascript` result: `None` and `""` are false.
fn truthy(result: &Option<String>) -> bool {
    result.as_deref().is_some_and(|s| !s.is_empty())
}

/// Python `result or default`.
fn or_default<'a>(result: &'a Option<String>, default: &'a str) -> &'a str {
    match result.as_deref() {
        Some(s) if !s.is_empty() => s,
        _ => default,
    }
}

/// Python `repr()` of a str, for the `%r` in log lines.
pub fn py_repr(s: &str) -> String {
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
            c if !is_printable(c) => {
                let n = c as u32;
                if n < 0x100 {
                    out.push_str(&format!("\\x{n:02x}"));
                } else if n < 0x10000 {
                    out.push_str(&format!("\\u{n:04x}"));
                } else {
                    out.push_str(&format!("\\U{n:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `str.isprintable()` for one char: every general category is
/// printable except Cc, Cf, Cs, Co, Cn, Zl, Zp and Zs; the ASCII space is
/// printable.
fn is_printable(c: char) -> bool {
    use unicode_general_category::{get_general_category, GeneralCategory as G};
    if c == ' ' {
        return true;
    }
    !matches!(
        get_general_category(c),
        G::Control
            | G::Format
            | G::Surrogate
            | G::PrivateUse
            | G::Unassigned
            | G::LineSeparator
            | G::ParagraphSeparator
            | G::SpaceSeparator
    )
}

/// `os.path.basename(cwd.rstrip("/")) if cwd else ""`.
pub fn needle_for(cwd: &str) -> String {
    if cwd.is_empty() {
        return String::new();
    }
    let trimmed = cwd.trim_end_matches('/');
    trimmed.rsplit('/').next().unwrap_or("").to_string()
}

/// Best-effort raise of the terminal for `cwd`. Never fails.
pub async fn focus_session_terminal(cwd: &str) {
    focus_session_terminal_with(&SystemBackend, cwd, &activate_targets()).await
}

/// `focus_session_terminal` over an explicit backend and activate list (the
/// seams the Python tests monkeypatch). Never fails.
pub async fn focus_session_terminal_with(
    backend: &dyn FocusBackend,
    cwd: &str,
    targets: &[ActivateTarget],
) {
    let needle = needle_for(cwd);
    if let Err(e) = focus_inner(backend, &needle, targets).await {
        tracing::warn!("focus: failed non-fatally: {}", e);
    }
}

async fn focus_inner(
    backend: &dyn FocusBackend,
    needle: &str,
    targets: &[ActivateTarget],
) -> anyhow::Result<()> {
    let needle_arg = if needle.is_empty() { "~" } else { needle };
    let needle_r = py_repr(needle);

    if backend.app_running("iTerm2").await? {
        let result = backend.osascript(ITERM_SCRIPT, &[needle_arg]).await?;
        tracing::info!("focus: iTerm2 {} (needle={})", or_default(&result, "error"), needle_r);
        if truthy(&result) {
            return Ok(());
        }
    }
    if backend.app_running("Terminal").await? {
        let result = backend.osascript(TERMINAL_SCRIPT, &[needle_arg]).await?;
        tracing::info!("focus: Terminal {} (needle={})", or_default(&result, "error"), needle_r);
        if truthy(&result) {
            return Ok(());
        }
    }

    let mut running: Vec<&ActivateTarget> = Vec::new();
    for target in targets {
        if backend.app_running(&target.0).await? {
            running.push(target);
        }
    }

    // Pass 1: window-title match across every running candidate — the
    // session might live in the second app on the list, and raising the
    // right window beats raising a whole app.
    let mut frontmost_app: Option<&str> = None;
    if !needle.is_empty() {
        for (proc_name, _) in &running {
            let result = backend.osascript(AX_RAISE_SCRIPT, &[proc_name, needle]).await?;
            match result.as_deref() {
                Some("raised") => {
                    tracing::info!("focus: raised {} window matching {}", proc_name, needle_r);
                    return Ok(());
                }
                Some("frontmost") if frontmost_app.is_none() => {
                    frontmost_app = Some(proc_name);
                }
                _ => {}
            }
        }
    }

    // A candidate is already frontmost and nothing matched by title.
    // Claude Code retitles windows to the conversation summary, which
    // never contains the cwd, so this is the common case — and "frontmost"
    // does NOT mean visible: an app whose only window is minimized, or
    // whose windows all live on another Space, stays the active app while
    // the human looks at something else (every tap 2026-09-05 landed here
    // and did nothing). Prove a window is on screen before standing down.
    if let Some(app) = frontmost_app {
        let unmin = backend.osascript(UNMINIMIZE_SCRIPT, &[app]).await?;
        if unmin.as_deref() == Some("visible") {
            tracing::info!(
                "focus: {} already frontmost with a window on screen, no window matched {}",
                app,
                needle_r
            );
            return Ok(());
        }
        if unmin.as_deref() == Some("unminimized") {
            tracing::info!("focus: {} frontmost but minimized — restored a window", app);
            return Ok(());
        }
        // No window on this Space (or AX error): activate by bundle id,
        // which switches to the Space holding the app's windows.
        let bundle_id = running
            .iter()
            .find(|(n, _)| n == app)
            .and_then(|(_, b)| b.as_deref());
        let result = match bundle_id {
            Some(bid) => backend.osascript(ACTIVATE_SCRIPT, &[bid]).await?,
            None => backend.osascript(ACTIVATE_BY_NAME_SCRIPT, &[app]).await?,
        };
        tracing::info!(
            "focus: {} frontmost with no visible window ({}) — activated ({})",
            app,
            or_default(&unmin, "ax-error"),
            or_default(&result, "error")
        );
        return Ok(());
    }

    // Pass 2: no window matched anywhere — raise the first running app,
    // restoring a minimized window if that's all it has (activate alone
    // leaves a Dock-minimized window in the Dock).
    for (proc_name, bundle_id) in &running {
        let result = match bundle_id {
            Some(bid) => backend.osascript(ACTIVATE_SCRIPT, &[bid]).await?,
            None => backend.osascript(ACTIVATE_BY_NAME_SCRIPT, &[proc_name]).await?,
        };
        if truthy(&result) {
            let unmin = backend.osascript(UNMINIMIZE_SCRIPT, &[proc_name]).await?;
            tracing::info!(
                "focus: activated {} (windows: {})",
                proc_name,
                or_default(&unmin, "ax-error")
            );
            return Ok(());
        }
        tracing::info!("focus: activated {} (error)", proc_name);
    }
    tracing::info!("focus: no known terminal app running (needle={})", needle_r);
    Ok(())
}

#[cfg(test)]
mod tests;
