//! `code_origin()` and `_log_permission_config_summary(matchers)`: what the daemon logs as it starts.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::board::py_repr;

/// `subprocess.run([...], capture_output=True, text=True, timeout=3).stdout.strip() or "?"`. Err is the
/// exception Python's `run` raises: git missing, or the timeout.
fn git(here: &Path, args: &[&str]) -> std::io::Result<String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(here)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "git timed out after 3 seconds"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let out = child.wait_with_output()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Ok(if text.is_empty() { "?".into() } else { text })
}

/// The directory the running code came from (Python's `Path(__file__).resolve().parent`): this crate's source
/// directory, in the same checkout as the Python package.
fn package_dir() -> PathBuf {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.canonicalize().unwrap_or(here)
}

/// `code_origin()`: where the running code came from, for the start-up log line: the checkout's branch and short
/// commit (`git -C <package dir> ...`), or "installed package" outside a checkout.
///
/// Which checkout and commit this daemon loaded: "<repo root> @ <short sha> (<branch>)". Logged at start, so a
/// daemon running another worktree's code is visible on its first lines (2026-10-08: worktrees sharing one .venv
/// re-pointed the editable install, and the live daemon ran a half-built branch with every /api/link path a 404).
pub fn code_origin() -> String {
    let here = package_dir();
    let parts = (|| -> std::io::Result<(String, String, String)> {
        Ok((
            git(&here, &["rev-parse", "--show-toplevel"])?,
            git(&here, &["rev-parse", "--short", "HEAD"])?,
            git(&here, &["rev-parse", "--abbrev-ref", "HEAD"])?,
        ))
    })();
    match parts {
        Ok((root, sha, branch)) => format!("{root} @ {sha} ({branch})"),
        // not a git checkout (an installed binary): say where the code is
        Err(_) => here.display().to_string(),
    }
}

/// `len(x or [])` for a JSON value, as Python counts it.
fn py_len(v: Option<&Value>) -> usize {
    match v {
        Some(Value::Array(a)) => a.len(),
        Some(Value::Object(m)) => m.len(),
        Some(Value::String(s)) => s.chars().count(),
        _ => 0,
    }
}

/// `_log_permission_config_summary(matchers)`: one INFO line per matcher class, and the Claude config homes the
/// hooks are installed in (claude_home.claude_config_dirs).
///
/// One-shot log at startup: how does the matcher interact with Claude Code's
/// own permissions config? Flags the two most confusing misalignments:
///
/// 1. defaultMode == 'bypassPermissions' AND matcher is non-strict — only
///    always_ask patterns are asked (on the phone, with the relay on); everything
///    else is silently bypassed.
/// 2. matcher.strict but defaultMode unsuitable — strict mode wants
///    bypassPermissions, otherwise unmatched commands still go through Claude
///    Code's normal prompt UI.
pub fn log_permission_config_summary(matchers: &eve_matchers::MatcherConfig) {
    // The daemon serves every config home it was pointed at, so summarize the
    // first one that actually exists rather than assuming ~/.claude.
    let candidates: Vec<PathBuf> = match eve_claude_home::claude_config_dirs(None) {
        Ok(dirs) => dirs.into_iter().map(|d| d.join("settings.json")).collect(),
        Err(e) => {
            tracing::warn!("permission summary: no Claude config home ({e})");
            return;
        }
    };
    let Some(first) = candidates.first().cloned() else {
        return;
    };
    let settings_path = candidates.iter().find(|p| p.exists()).cloned().unwrap_or(first);
    let mut default_mode: Option<Value> = None;
    let mut ask_count = 0usize;
    if settings_path.exists() {
        match std::fs::read(&settings_path).map_err(|e| e.to_string()).and_then(|b| {
            eve_ipc::pyjson::loads(&String::from_utf8_lossy(&b)).map_err(|e| e.to_string())
        }) {
            Ok(data) => {
                let perms = data.get("permissions").filter(|p| crate::board::truthy(Some(p)));
                default_mode = perms.and_then(|p| p.get("defaultMode")).filter(|v| !v.is_null()).cloned();
                ask_count = py_len(perms.and_then(|p| p.get("ask")));
            }
            Err(e) => tracing::debug!("could not read settings.json for permission summary: {}", e),
        }
    }

    let matcher_summary = format!(
        "matcher: strict={} auto_allow={} always_ask={}",
        if matchers.strict { "True" } else { "False" },
        matchers.auto_allow.len(),
        matchers.always_ask.len()
    );
    tracing::info!("{}", matcher_summary);
    tracing::info!("code: {}", code_origin());
    let shown = default_mode
        .clone()
        .filter(|v| crate::board::truthy(Some(v)))
        .unwrap_or_else(|| Value::from("(unset)"));
    tracing::info!(
        "settings.json ({}): permissions.defaultMode={} ask={}",
        settings_path.display(),
        py_repr(&shown),
        ask_count
    );

    let bypass = default_mode.as_ref().and_then(Value::as_str) == Some("bypassPermissions");
    if bypass && !matchers.strict {
        tracing::warn!(
            "permissions.defaultMode='bypassPermissions' + matcher.strict=false: \
             only always_ask patterns ({} defined) are asked, on the phone with the \
             Telegram relay on; everything else is auto-approved without any \
             human-in-the-loop. To ask about every un-vetted command, set \
             `strict = true` in your matchers.toml.",
            matchers.always_ask.len()
        );
    } else if matchers.strict && !bypass && default_mode.is_some() {
        let mode = py_repr(default_mode.as_ref().unwrap_or(&Value::Null));
        tracing::warn!(
            "matcher.strict=true but permissions.defaultMode={}: unmatched \
             commands are asked on the phone (relay on) AND Claude Code may still surface \
             its own terminal prompt depending on the mode. Strict mode is \
             designed to pair with defaultMode='bypassPermissions'.",
            mode
        );
    }
}
