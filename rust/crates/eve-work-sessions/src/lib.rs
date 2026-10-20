//! The Work tab's sessions: Claude Code in `~/Documents/personal/work`, started and driven from the phone.
//! Port of `eve_bridge/work_sessions.py`.
//!
//! Owner, 2026-10-12: "can work essentially be a new terminal session in documents personal", "since its long
//! horizon", "make a folder called work under personal where this happens". Each session is plain
//! `claude --dangerously-skip-permissions` in that one folder (`work_root`; `EVE_WORK_ROOT` moves it), never
//! claude_launch's "work" *area* (`~/Documents/work` with era-code). The routes are link_chat's
//! `/api/link/sessions*`; the whole design is in docs/work-sessions.md.
//!
//! **Start.** `start(task, client_id)` mints the session's id (a UUID), writes the task to `<sid>.task` and eve's
//! MCP config to `<sid>.mcp.json` (both 0600, in `~/.config/eve-bridge/work/`, 0700), and opens one Terminal.app
//! window whose only line is code-made: `cd <root> && exec <python> -P -m eve_bridge.work_sessions launch <sid>`
//! (`-P`: the shared work folder, where sessions write files, is never on `sys.path`; the MCP server's args carry
//! it too), or `cd <root> && exec <launcher> launch <sid>` for this crate's own binary ([`Launch`]).
//! That line is passed to osascript as an argument, never written into the script's source. `launch` ([`main`])
//! reads and deletes the task file and replaces itself with `claude`, the task as one argv element after `--`
//! ([`launch_argv`]): the task never passes through a shell or an AppleScript string, whatever it holds. The same
//! `client_id` within [`DEDUPE_SECS`] answers the first start's session; nothing else opens.
//!
//! **Which session is which.** Every session shares one folder, so the folder cannot tell them apart. The id can:
//! `claude --session-id <sid>` makes Claude Code's own session id ours, so the hooks' `session_id` (`note`) and
//! `ps` ([`session_pids`]) both name it. `ps` finding the process is liveness and the stop target. A `/clear`
//! inside the session goes on under a new session id in the same process: the row stays (ps still names ours), its
//! state and detail stop following, and the new id is listed beside it as a session eve did not start.
//!
//! **Reply.** Terminal.app's `do script <text> in <tab>` types into that one tab, found by its window id and its tty
//! (kept at start), with the text as an osascript argument. It is refused unless `ps` finds the session's claude and
//! the tab is busy (a program, not a shell prompt, holds it; `exec` means no shell is left once claude exits). No
//! other window is touched and nothing needs focus. Warp has no AppleScript tab addressing, so Work sessions open in
//! Terminal.app whatever `EVE_CLAUDE_TERMINAL` says for "new claude".
//!
//! **Stop.** SIGTERM to the pid `ps` gives for `--session-id <sid>`, SIGKILL after [`STOP_WAIT_SECS`]. Idempotent.
//!
//! **The list** is every live Claude Code session the daemon knows (`state.sessions`) plus the Work ones; only Work
//! sessions can be replied to or stopped. A session's state: waiting on you (a Notification that waits, or an
//! AskUserQuestion), working (between a prompt and Stop), idle, or starting (opened, no hook yet).
//!
//! **Detail** answers state "ended" (and `ended: true`) for a session that is over, and ends one whose process `ps`
//! no longer finds after [`GRACE_SECS`], as the list does. A 409 for an ended session carries `code: "ended"`.
//!
//! **Detail** reads the transcript's last [`TAIL_BYTES`] on a thread: the owner's prompts and Claude's text, never
//! tool calls, tool results or thinking, each clipped to [`TURN_CHARS`] with token-shaped strings and `KEY=value`
//! secrets masked ([`redact`]). Folders are basenames only.
//!
//! Logs carry ids' first 8 characters, counts and states, never a task, a reply or a transcript line.

pub mod py;

use std::collections::HashMap;
use std::future::Future;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use base64::Engine as _;
use eve_claude_live::{RunError, RunOutput};
use eve_state::State;
use indexmap::IndexMap;
use regex::Regex;
use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

pub use eve_claude_live::{PROBE_TIMEOUT_SECS, PS};

pub const REGISTRY_NAME: &str = "work-sessions.json";
/// `<sid>.task` and `<sid>.mcp.json`, 0600, in a 0700 folder.
pub const FILES_NAME: &str = "work";
pub const MAX_TASK_CHARS: usize = 4000;
pub const MAX_REPLY_CHARS: usize = 4000;
pub const MAX_CLIENT_ID_CHARS: usize = 64;
pub const TITLE_CHARS: usize = 80;
pub const LINE_CHARS: usize = 200;
pub const TURN_CHARS: usize = 2000;
pub const TURNS_SHOWN: usize = 20;
/// Detail reads at most this much of the transcript's end.
pub const TAIL_BYTES: u64 = 512 * 1024;
/// The list's last line, from at most this much.
pub const LINE_TAIL_BYTES: u64 = 64 * 1024;
/// The same client_id within this is the same session.
pub const DEDUPE_SECS: f64 = 600.0;
/// A session this new is "starting" whatever ps says.
pub const GRACE_SECS: f64 = 30.0;
/// SIGTERM, then SIGKILL after this.
pub const STOP_WAIT_SECS: f64 = 5.0;
/// An ended session's record is kept this long.
pub const KEEP_SECS: f64 = (7 * 24 * 3600) as f64;
pub const MAX_LISTED: usize = 30;
/// Live states; "ended" only in detail.
pub const STATES: [&str; 4] = ["starting", "working", "waiting", "idle"];
/// The "code" beside ENDED, so the app needs no display text to match.
pub const ENDED_CODE: &str = "ended";
pub const OFF_LINE: &str = "Work sessions are off on this Mac (EVE_WORK_SESSIONS=0).";
pub const ENDED: &str = "that session has ended";
pub const NO_WINDOW: &str = "can't reach that session's window";
pub const NOT_OURS: &str = "eve can only reply to or stop sessions it started from Work";
pub const UNKNOWN: &str = "there is no such session";
pub const MASK: &str = "[hidden]";

/// Terminal.app, with every value an argument (`on run argv`), never part of the script's text.
/// Opens one window running item 1, and answers "<window id> <tty>" for the tab it made.
/// "0" for a window id it could not read: the window is open, but replies cannot find it.
pub const TERMINAL_OPEN: &str = r#"on run argv
  tell application "Terminal"
    set t to do script (item 1 of argv)
    activate
    set wid to 0
    try
      repeat with w in windows
        if (tabs of w) contains t then
          set wid to id of w
          exit repeat
        end if
      end repeat
    end try
    if wid is 0 then
      try
        set wid to id of front window
      end try
    end if
    set tt to ""
    try
      set tt to tty of t
    end try
    return (wid as string) & " " & tt
  end tell
end run"#;
/// Types item 3 into the tab of window item 1 whose tty is item 2, only while a program holds it (busy).
pub const TERMINAL_TYPE: &str = r#"on run argv
  set wid to (item 1 of argv) as integer
  tell application "Terminal"
    if not (exists window id wid) then return "gone"
    repeat with t in tabs of window id wid
      if (tty of t) is (item 2 of argv) then
        if not (busy of t) then return "idle"
        do script (item 3 of argv) in t
        return "ok"
      end if
    end repeat
    return "gone"
  end tell
end run"#;

/// The environment Python's `environ` parameter stands for: `None` is the process environment.
pub type Environ = HashMap<String, String>;

fn env_get(environ: Option<&Environ>, key: &str) -> Option<String> {
    match environ {
        Some(env) => env.get(key).cloned(),
        None => std::env::var_os(key).map(|v| v.to_string_lossy().into_owned()),
    }
}

/// `Path.home()`.
fn home_dir() -> PathBuf {
    eve_claude_home::HomeEnv::from_process().home_dir().unwrap_or_else(|_| PathBuf::from("~"))
}

/// `~/.config/eve-bridge` (Python's `CONFIG_DIR`).
pub fn config_dir() -> PathBuf {
    home_dir().join(".config").join("eve-bridge")
}

/// `EVE_WORK_SESSIONS`: on unless it says 0, off, false or no.
pub fn enabled(environ: Option<&Environ>) -> bool {
    let raw = env_get(environ, "EVE_WORK_SESSIONS").unwrap_or_else(|| "1".to_string());
    !matches!(py::strip(&raw).to_lowercase().as_str(), "0" | "off" | "false" | "no")
}

/// `~/Documents/personal/work`, or `EVE_WORK_ROOT`. Never claude_launch's work area.
pub fn work_root(environ: Option<&Environ>) -> PathBuf {
    let raw = env_get(environ, "EVE_WORK_ROOT").unwrap_or_default();
    let raw = py::strip(&raw);
    if raw.is_empty() {
        return home_dir().join("Documents").join("personal").join("work");
    }
    eve_claude_home::HomeEnv::from_process()
        .expanduser(Path::new(raw))
        .unwrap_or_else(|_| py::normalize(Path::new(raw)))
}

/// `<home>/work`, `home` defaulting to [`config_dir`].
pub fn files_dir(home: Option<&Path>) -> PathBuf {
    match home {
        Some(h) => py::normalize(h).join(FILES_NAME),
        None => config_dir().join(FILES_NAME),
    }
}

/// One line, at most `limit` characters (an ellipsis when cut).
pub fn flat(text: &str, limit: usize) -> String {
    let line = py::join_words(text);
    if py::len(&line) <= limit {
        return line;
    }
    format!("{}…", py::rstrip(py::prefix(&line, limit.saturating_sub(1))))
}

/// Stripped, at most `limit` characters (an ellipsis when cut).
pub fn clip(text: &str, limit: usize) -> String {
    let text = py::strip(text);
    if py::len(text) <= limit {
        return text.to_string();
    }
    format!("{}…", py::rstrip(py::prefix(text, limit.saturating_sub(1))))
}

/// `UUID_RE.match(s)`: a lowercase hyphenated UUID; like Python's `$`, a single trailing newline is allowed.
pub fn is_uuid(s: &str) -> bool {
    static UUID_RE: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"\A[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}\n?\z").expect("valid regex")
    });
    UUID_RE.is_match(s)
}

// ---- redact ------------------------------------------------------------------------------------------------

// Token-shaped strings and KEY=value secrets, masked before a transcript line reaches the phone.
static SECRET_PATTERNS: LazyLock<Vec<Regex>> = LazyLock::new(|| {
    [
        r"\b(?:sk|pk|rk)-[A-Za-z0-9_-]{16,}",                       // API keys (OpenAI, OpenRouter, ...)
        r"\bgh[pousr]_[A-Za-z0-9]{20,}",                            // GitHub tokens
        r"\bgithub_pat_[A-Za-z0-9_]{20,}",
        r"\bxox[abprs]-[A-Za-z0-9-]{10,}",                          // Slack
        r"\bAKIA[0-9A-Z]{16}\b",                                    // AWS access key ids
        r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}", // JWTs
        r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{16,}",
    ]
    .iter()
    .map(|p| Regex::new(p).expect("valid regex"))
    .collect()
});
static PEM_BEGIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").expect("valid regex"));
static PEM_END: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-----END [A-Z ]*PRIVATE KEY-----").expect("valid regex"));
static KEY_VALUE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?im)\b([A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|PASSWD)[A-Z0-9_]*)\s*[=:]\s*("[^"\n]*"|'[^'\n]*'|\S+)"#,
    )
    .expect("valid regex")
});
static ONE_DIGIT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\A\d\z").expect("valid regex"));

/// Python's `-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)`: a private key
/// block to its END line, or to the end of the text (before a final newline, as `$` matches).
fn mask_pem(text: &str) -> String {
    let mut out = String::new();
    let mut pos = 0;
    while let Some(m) = PEM_BEGIN.find_at(text, pos) {
        let dollar = if text.ends_with('\n') && text.len() > m.end() { text.len() - 1 } else { text.len() };
        let end = match PEM_END.find_at(text, m.end()) {
            Some(e) if e.start() <= dollar => e.end(),
            _ => dollar,
        };
        out.push_str(&text[pos..m.start()]);
        out.push_str(MASK);
        pos = end;
        if pos >= text.len() {
            break;
        }
    }
    out.push_str(&text[pos..]);
    out
}

/// Python's `\w` on a str.
fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn in_class(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '-'
}

/// Python's `\b(?=[A-Za-z0-9_-]*\d)(?=[A-Za-z0-9_-]*[A-Za-z])[A-Za-z0-9_-]{40,}\b` (the regex crate has no
/// lookahead): a run of 40 or more such characters holding a digit and a letter, ending at a word boundary.
fn mask_long_random(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let n = chars.len();
    let boundary = |i: usize| -> bool {
        let before = i > 0 && is_word(chars[i - 1]);
        let after = i < n && is_word(chars[i]);
        before != after
    };
    let mut out = String::new();
    let mut i = 0;
    let mut copied = 0;
    while i < n {
        if boundary(i) && in_class(chars[i]) {
            let mut run_end = i;
            while run_end < n && in_class(chars[run_end]) {
                run_end += 1;
            }
            let digit = chars[i..run_end].iter().any(|c| c.is_ascii_digit())
                || (run_end < n && ONE_DIGIT.is_match(chars[run_end].encode_utf8(&mut [0; 4])));
            let letter = chars[i..run_end].iter().any(|c| c.is_ascii_alphabetic());
            if digit && letter && run_end - i >= 40 {
                if let Some(k) = (i + 40..=run_end).rev().find(|&k| boundary(k)) {
                    out.extend(&chars[copied..i]);
                    out.push_str(MASK);
                    copied = k;
                    i = k;
                    continue;
                }
            }
        }
        i += 1;
    }
    out.extend(&chars[copied..]);
    out
}

/// Token-shaped strings and `KEY=value` secrets masked.
pub fn redact(text: &str) -> String {
    let mut out = text.to_string();
    for pattern in SECRET_PATTERNS.iter() {
        out = pattern.replace_all(&out, MASK).into_owned();
    }
    out = mask_pem(&out);
    out = KEY_VALUE.replace_all(&out, |c: &regex::Captures<'_>| format!("{}={MASK}", &c[1])).into_owned();
    mask_long_random(&out)
}

// ---- transcripts -------------------------------------------------------------------------------------------

/// Where Claude Code writes a session's transcript: `<config>/projects/<cwd, "/" and "." as "-">/<sid>.jsonl`.
/// `config_dir` None is `claude_home.claude_config_dir()`.
pub fn derived_transcript(
    cwd: &Path,
    sid: &str,
    config_dir: Option<&Path>,
) -> Result<PathBuf, eve_claude_home::HomeDirError> {
    let base = match config_dir {
        Some(d) => py::normalize(d),
        None => eve_claude_home::claude_config_dir(None)?,
    };
    let name: String =
        py::normalize(cwd).to_string_lossy().chars().map(|c| if c == '/' || c == '.' { '-' } else { c }).collect();
    Ok(base.join("projects").join(name).join(format!("{sid}.jsonl")))
}

/// `datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp()`, 0.0 for anything else.
fn epoch(stamp: Option<&Value>) -> f64 {
    match stamp {
        Some(Value::String(s)) if !s.is_empty() => py::iso_timestamp(s).unwrap_or(0.0),
        _ => 0.0,
    }
}

/// The words in one message's content: plain text only (no tool_use, tool_result, thinking or images).
fn texts(content: Option<&Value>) -> Vec<String> {
    match content {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(blocks)) => blocks
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str).map(str::to_string))
            .collect(),
        _ => Vec::new(),
    }
}

/// The JSON objects in the last `limit` bytes of a JSONL file, and whether earlier lines were left out.
pub fn read_tail(path: &Path, limit: u64) -> (Vec<Map<String, Value>>, bool) {
    let read = || -> std::io::Result<(Vec<u8>, u64)> {
        let mut f = std::fs::File::open(path)?;
        let size = f.seek(SeekFrom::End(0))?;
        let start = size.saturating_sub(limit);
        f.seek(SeekFrom::Start(start))?;
        let mut raw = Vec::new();
        f.take(limit).read_to_end(&mut raw)?;
        Ok((raw, start))
    };
    let Ok((raw, start)) = read() else {
        return (Vec::new(), false);
    };
    let mut lines: Vec<&[u8]> = raw.split(|b| *b == b'\n').collect();
    if start > 0 && !lines.is_empty() {
        lines.remove(0); // the first line was cut in half
    }
    let mut out = Vec::new();
    for line in lines {
        if line.iter().all(|b| b.is_ascii_whitespace() || *b == 0x0b) {
            continue;
        }
        // json.loads on bytes reads a UTF-8 BOM as an encoding mark.
        let body = line.strip_prefix(b"\xef\xbb\xbf").unwrap_or(line);
        if let Ok(Value::Object(obj)) = serde_json::from_slice::<Value>(body) {
            out.push(obj);
        }
    }
    (out, start > 0)
}

/// One turn of a transcript: `{"role": "owner" | "claude", "text", "at"}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Turn {
    pub role: String,
    pub text: String,
    pub at: i64,
}

/// [`transcript_turns_with`] with the defaults: [`TURNS_SHOWN`], [`TAIL_BYTES`], [`TURN_CHARS`].
pub fn transcript_turns(path: Option<&Path>) -> (Vec<Turn>, bool) {
    transcript_turns_with(path, TURNS_SHOWN, TAIL_BYTES, TURN_CHARS)
}

/// The last `limit` turns, oldest first, and whether more came before. The owner's typed prompts and Claude's
/// text only: subagents' lines, meta lines, command and system wrappers (text that starts with "<"), tool calls,
/// tool results and thinking are left out.
pub fn transcript_turns_with(path: Option<&Path>, limit: usize, tail: u64, chars: usize) -> (Vec<Turn>, bool) {
    let Some(path) = path else {
        return (Vec::new(), false);
    };
    let (objs, cut) = read_tail(path, tail);
    let mut turns: Vec<Turn> = Vec::new();
    for obj in &objs {
        if obj.get("isSidechain").is_some_and(py::truthy) || obj.get("isMeta").is_some_and(py::truthy) {
            continue;
        }
        let Some(Value::Object(msg)) = obj.get("message") else {
            continue;
        };
        let role = msg.get("role").and_then(Value::as_str);
        let Some(role @ ("user" | "assistant")) = role else {
            continue;
        };
        match obj.get("type") {
            None | Some(Value::Null) => {}
            Some(Value::String(t)) if t == "user" || t == "assistant" => {}
            _ => continue,
        }
        for text in texts(msg.get("content")) {
            let text = py::strip(&text);
            if text.is_empty() || (role == "user" && text.starts_with('<')) {
                continue;
            }
            turns.push(Turn {
                role: if role == "user" { "owner" } else { "claude" }.to_string(),
                text: clip(&redact(text), chars),
                at: epoch(obj.get("timestamp")) as i64,
            });
        }
    }
    let more = turns.len() > limit;
    // `turns[-limit:]`, where a limit of 0 keeps every turn.
    if limit > 0 && more {
        turns.drain(..turns.len() - limit);
    }
    (turns, cut || more)
}

/// Claude's newest text in the transcript, one line, at most [`LINE_CHARS`].
pub fn last_line(path: Option<&Path>) -> String {
    let (turns, _) = transcript_turns_with(path, TURNS_SHOWN, LINE_TAIL_BYTES, LINE_CHARS * 2);
    turns.iter().rev().find(|t| t.role == "claude").map(|t| flat(&t.text, LINE_CHARS)).unwrap_or_default()
}

fn mtime(path: Option<&Path>) -> f64 {
    path.and_then(|p| std::fs::metadata(p).ok())
        .and_then(|m| m.modified().ok())
        .map(|t| match t.duration_since(std::time::UNIX_EPOCH) {
            Ok(d) => d.as_secs_f64(),
            Err(e) => -e.duration().as_secs_f64(),
        })
        .unwrap_or(0.0)
}

// ---- the process -------------------------------------------------------------------------------------------

/// `run(argv, timeout)` → ps's output: Python's `subprocess.run` seam, shareable across threads.
pub type RunFn = Arc<dyn Fn(&[String], Duration) -> Result<RunOutput, RunError> + Send + Sync>;

/// Each running Claude Code process started with `--session-id <uuid>` → its pid; None when ps fails.
pub fn session_pids(run: eve_claude_live::Run<'_>) -> Option<IndexMap<String, i64>> {
    let argv: Vec<String> = [PS, "-axww", "-o", "pid=,args="].iter().map(|s| s.to_string()).collect();
    let ps = match run(&argv, Duration::from_secs_f64(PROBE_TIMEOUT_SECS)) {
        Ok(out) => out,
        Err(e) => {
            tracing::debug!("work sessions: ps failed ({})", e.type_name());
            return None;
        }
    };
    if ps.returncode != 0 {
        return None;
    }
    let mut out = IndexMap::new();
    for line in py::splitlines(&ps.stdout) {
        let line = py::strip(line);
        let (pid, args) = line.split_once(' ').unwrap_or((line, ""));
        if pid.is_empty() || !pid.chars().all(|c| c.is_ascii_digit()) || !eve_claude_live::is_claude(py::strip(args))
        {
            continue;
        }
        let Ok(pid) = pid.parse::<i64>() else {
            continue;
        };
        let words = py::split(args);
        for (i, word) in words.iter().enumerate() {
            if *word == "--" {
                break; // what follows is the task, never a flag
            }
            let sid = if *word == "--session-id" && i + 1 < words.len() {
                words[i + 1]
            } else {
                word.strip_prefix("--session-id=").unwrap_or_default()
            };
            if is_uuid(sid) {
                out.insert(sid.to_string(), pid);
                break;
            }
        }
    }
    Some(out)
}

/// A string that is not a session id, where a session id is needed (Python's `ValueError("not a session id")`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("not a session id")]
pub struct NotASessionId;

/// Claude Code's argv for a Work session. `--mcp-config` takes several values, so `--session-id` follows it,
/// and `--` ends the options: the task is the one positional argument (the first prompt), even when it starts
/// with a dash.
pub fn launch_argv(sid: &str, task: &str, mcp_config: &Path, claude: &str) -> Result<Vec<String>, NotASessionId> {
    if !is_uuid(sid) {
        return Err(NotASessionId);
    }
    Ok(vec![
        claude.to_string(),
        "--dangerously-skip-permissions".to_string(),
        "--mcp-config".to_string(),
        mcp_config.to_string_lossy().into_owned(),
        "--session-id".to_string(),
        sid.to_string(),
        "--".to_string(),
        task.to_string(),
    ])
}

fn mcp_server(sid: &str, token: &str, command: &str, args: Value, environ: Option<&Environ>) -> Value {
    let mut server_env = Map::new();
    server_env.insert("EVE_MCP_SESSION".into(), Value::from(sid));
    server_env.insert("EVE_MCP_TOKEN".into(), Value::from(token));
    if let Some(sock) = env_get(environ, "EVE_BRIDGE_SOCK").filter(|s| !s.is_empty()) {
        server_env.insert("EVE_BRIDGE_SOCK".into(), Value::from(sock));
    }
    json!({"mcpServers": {"eve": {"type": "stdio", "command": command, "args": args, "env": Value::Object(server_env)}}})
}

/// eve's MCP server for one session (eve_mcp.py): it speaks to the running daemon with this session's token.
pub fn mcp_config(sid: &str, token: &str, python: &str, environ: Option<&Environ>) -> Value {
    mcp_server(sid, token, python, json!(["-P", "-m", "eve_bridge.eve_mcp"]), environ)
}

/// [`mcp_config`] for a native eve MCP server binary: `command` with no arguments.
pub fn mcp_config_native(sid: &str, token: &str, command: &str, environ: Option<&Environ>) -> Value {
    mcp_server(sid, token, command, json!([]), environ)
}

/// Python's `path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)`: the parents with the default mode,
/// the folder itself 0700.
fn mkdir_private(dir: &Path) -> std::io::Result<()> {
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(unix)]
fn chmod(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn chmod(_path: &Path, _mode: u32) -> std::io::Result<()> {
    Ok(())
}

/// Write `text` to a 0600 file in a 0700 folder.
pub fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    mkdir_private(parent)?;
    let _ = chmod(parent, 0o700);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(text.as_bytes())?;
    drop(f);
    chmod(path, 0o600)
}

type BoxFuture<T> = Pin<Box<dyn Future<Output = T> + Send + 'static>>;

/// `osascript(script, *argv)` → (exit code, its stdout or stderr); an `Err` is the `OSError` Python catches.
pub type Osascript = Arc<dyn Fn(String, Vec<String>) -> BoxFuture<std::io::Result<(i32, String)>> + Send + Sync>;

/// osascript with `script` as its source and `args` as `argv` → (exit code, its stdout or stderr).
pub async fn osascript(script: String, args: Vec<String>) -> std::io::Result<(i32, String)> {
    let out = tokio::process::Command::new("osascript")
        .arg("-e")
        .arg(&script)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .output()
        .await?;
    let code = match out.status.code() {
        Some(c) => c,
        None => {
            #[cfg(unix)]
            {
                use std::os::unix::process::ExitStatusExt;
                out.status.signal().map(|s| -s).unwrap_or(0)
            }
            #[cfg(not(unix))]
            {
                0
            }
        }
    };
    let bytes = if code == 0 { &out.stdout } else { &out.stderr };
    let text = String::from_utf8_lossy(bytes);
    Ok((code, py::prefix(py::strip(&text), 200).to_string()))
}

/// sha256 hex of a token: the registry keeps only this.
pub fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

/// `hmac.compare_digest` on two ASCII strings: constant time in their content.
fn compare_digest(a: &str, b: &str) -> bool {
    if !a.is_ascii() || !b.is_ascii() || a.len() != b.len() {
        return false;
    }
    a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// `secrets.token_urlsafe(32)`.
fn token_urlsafe() -> String {
    let bytes: [u8; 32] = rand::random();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

// ---- the registry ------------------------------------------------------------------------------------------

pub const SAVED_FIELDS: [&str; 10] =
    ["sid", "created_at", "title", "token_hash", "client_id", "window_id", "tty", "transcript_path", "stopped", "ended_at"];

/// One Work session eve started.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub sid: String,
    pub created_at: f64,
    pub title: String,
    pub token_hash: String,
    pub client_id: String,
    pub window_id: Option<i64>,
    pub tty: String,
    pub transcript_path: String,
    pub stopped: bool,
    pub ended_at: f64,
    /// Not saved: starting, working, waiting, idle.
    pub phase: String,
    /// Not saved: the newest hook.
    pub last_activity: f64,
}

impl Record {
    /// `Record(sid=..., created_at=..., title=..., token_hash=...)` with the dataclass defaults.
    pub fn new(sid: impl Into<String>, created_at: f64, title: impl Into<String>, token_hash: impl Into<String>) -> Self {
        Record {
            sid: sid.into(),
            created_at,
            title: title.into(),
            token_hash: token_hash.into(),
            client_id: String::new(),
            window_id: None,
            tty: String::new(),
            transcript_path: String::new(),
            stopped: false,
            ended_at: 0.0,
            phase: "starting".to_string(),
            last_activity: 0.0,
        }
    }

    pub fn over(&self) -> bool {
        self.stopped || self.ended_at > 0.0
    }

    /// The saved fields, in [`SAVED_FIELDS`] order.
    pub fn saved(&self) -> Value {
        let mut m = Map::new();
        m.insert("sid".into(), Value::from(self.sid.clone()));
        m.insert("created_at".into(), float_value(self.created_at));
        m.insert("title".into(), Value::from(self.title.clone()));
        m.insert("token_hash".into(), Value::from(self.token_hash.clone()));
        m.insert("client_id".into(), Value::from(self.client_id.clone()));
        m.insert("window_id".into(), self.window_id.map(Value::from).unwrap_or(Value::Null));
        m.insert("tty".into(), Value::from(self.tty.clone()));
        m.insert("transcript_path".into(), Value::from(self.transcript_path.clone()));
        m.insert("stopped".into(), Value::from(self.stopped));
        m.insert("ended_at".into(), float_value(self.ended_at));
        Value::Object(m)
    }

    /// One registry row as Python's `_load` reads it; None for a row it skips.
    fn from_row(row: &Value) -> Option<Record> {
        let row = row.as_object()?;
        let sid = match row.get("sid") {
            Some(Value::String(s)) if is_uuid(s) => s.clone(),
            _ => return None,
        };
        let window_id = match row.get("window_id") {
            None | Some(Value::Null) => None,
            Some(v) => Some(py::to_int(v)?),
        };
        let created_at = py::float_or_zero(row.get("created_at"))?;
        let mut rec = Record {
            sid,
            created_at,
            title: py::str_or_empty(row.get("title")),
            token_hash: py::str_or_empty(row.get("token_hash")),
            client_id: py::str_or_empty(row.get("client_id")),
            window_id,
            tty: py::str_or_empty(row.get("tty")),
            transcript_path: py::str_or_empty(row.get("transcript_path")),
            stopped: row.get("stopped").is_some_and(py::truthy),
            ended_at: py::float_or_zero(row.get("ended_at"))?,
            phase: "idle".to_string(),
            last_activity: 0.0,
        };
        rec.last_activity = rec.created_at;
        Some(rec)
    }
}

/// A float as a JSON number that keeps its float-ness (`1000.0`, never `1000`).
fn float_value(f: f64) -> Value {
    serde_json::Number::from_f64(f).map(Value::Number).unwrap_or(Value::Null)
}

// ---- the seams ---------------------------------------------------------------------------------------------

/// The daemon's state, or None (Python's `state: Callable[[], Any]`).
pub type StateFn = Arc<dyn Fn() -> Option<Arc<parking_lot::Mutex<State>>> + Send + Sync>;
/// `kill(pid, signal)`; an `Err` is the `OSError` `os.kill` raises.
pub type KillFn = Arc<dyn Fn(i64, i32) -> std::io::Result<()> + Send + Sync>;
/// The claude processes' folders → whether one there is interactive (claude_live.live_processes).
pub type LiveFn = Arc<dyn Fn() -> Option<IndexMap<String, bool>> + Send + Sync>;
/// `time.time()`.
pub type ClockFn = Arc<dyn Fn() -> f64 + Send + Sync>;
/// `asyncio.sleep`.
pub type SleepFn = Arc<dyn Fn(f64) -> BoxFuture<()> + Send + Sync>;

/// `os.kill`.
pub fn os_kill(pid: i64, sig: i32) -> std::io::Result<()> {
    let pid: libc::pid_t = pid.try_into().map_err(|_| std::io::Error::from_raw_os_error(libc::ESRCH))?;
    // SAFETY: kill(2) takes plain integers and touches no memory of ours.
    if unsafe { libc::kill(pid, sig) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// What a session's Terminal line starts, and what its eve MCP server runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launch {
    /// Python's own: `<python> -P -m eve_bridge.work_sessions launch <sid>`, and the MCP server
    /// `<python> -P -m eve_bridge.eve_mcp`.
    Python { python: String },
    /// This crate's binary: `<launcher> launch <sid>`, and the MCP server `<mcp>` with no arguments.
    Native { launcher: String, mcp: String },
}

impl Launch {
    /// The Terminal line for one session. Only code-made words: the folder (quoted), the program (quoted), the
    /// module and a checked UUID.
    pub fn line(&self, root: &Path, sid: &str) -> String {
        let root = py::shlex_quote(&root.to_string_lossy());
        match self {
            // -P: Python leaves the cwd (the shared work folder, where sessions write files) off sys.path, so a
            // json.py or uuid.py a session left there cannot shadow the stdlib or run in this interpreter.
            Launch::Python { python } => {
                format!("cd {root} && exec {} -P -m eve_bridge.work_sessions launch {sid}", py::shlex_quote(python))
            }
            Launch::Native { launcher, .. } => format!("cd {root} && exec {} launch {sid}", py::shlex_quote(launcher)),
        }
    }

    /// The session's eve MCP config.
    pub fn mcp_config(&self, sid: &str, token: &str, environ: Option<&Environ>) -> Value {
        match self {
            Launch::Python { python } => mcp_config(sid, token, python, environ),
            Launch::Native { mcp, .. } => mcp_config_native(sid, token, mcp, environ),
        }
    }
}

/// Everything that touches the Mac, injectable, as Python's `WorkSessions(...)` keywords.
pub struct Options {
    pub root: Option<PathBuf>,
    pub home: Option<PathBuf>,
    pub state: StateFn,
    pub osascript: Osascript,
    pub run: RunFn,
    pub kill: KillFn,
    /// None: `claude_live.live_processes(run)`.
    pub live: Option<LiveFn>,
    pub clock: ClockFn,
    pub launch: Launch,
    pub sleep: SleepFn,
    pub environ: Option<Environ>,
    pub claude_dir: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            root: None,
            home: None,
            state: Arc::new(|| None),
            osascript: Arc::new(|script, args| Box::pin(osascript(script, args))),
            run: Arc::new(eve_claude_live::subprocess_run),
            kill: Arc::new(os_kill),
            live: None,
            clock: Arc::new(eve_state::wall_time),
            launch: Launch::Python { python: "python3".to_string() },
            sleep: Arc::new(|secs| Box::pin(tokio::time::sleep(Duration::from_secs_f64(secs.max(0.0))))),
            environ: None,
            claude_dir: None,
        }
    }
}

/// A session the daemon knows from its hooks, snapshotted from the state for a worker thread.
#[derive(Debug, Clone, PartialEq)]
pub struct KnownSession {
    pub session_id: String,
    pub cwd: Option<String>,
    pub started_at: f64,
    pub running: bool,
    pub transcript_path: Option<String>,
    /// `state._needs_input_live(s)`.
    pub waiting: bool,
}

impl KnownSession {
    fn of(state: &State, s: &eve_state::Session) -> Self {
        KnownSession {
            session_id: s.session_id.clone(),
            cwd: s.cwd.clone(),
            started_at: s.started_at,
            running: s.running,
            transcript_path: s.transcript_path.clone(),
            waiting: state.needs_input_live(s),
        }
    }

    fn state(&self) -> &'static str {
        if self.waiting {
            "waiting"
        } else if self.running {
            "working"
        } else {
            "idle"
        }
    }
}

/// A route's answer: (HTTP status, JSON body).
pub type Answer = (u16, Value);

fn refuse(status: u16, reason: &str) -> Answer {
    (status, json!({"ok": false, "reason": reason}))
}

/// `asyncio.to_thread`: run on the blocking pool; a panic there surfaces here, as the exception would.
async fn to_thread<T, F>(f: F) -> T
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => v,
        Err(e) => match e.try_into_panic() {
            Ok(payload) => std::panic::resume_unwind(payload),
            Err(e) => panic!("work sessions: worker task cancelled: {e}"),
        },
    }
}

fn sid8(sid: &str) -> &str {
    py::prefix(sid, 8)
}

struct Inner {
    root: PathBuf,
    home: PathBuf,
    registry: PathBuf,
    files: PathBuf,
    state: StateFn,
    osascript: Osascript,
    run: RunFn,
    kill: KillFn,
    live: LiveFn,
    clock: ClockFn,
    launch: Launch,
    sleep: SleepFn,
    environ: Option<Environ>,
    claude_dir: Option<PathBuf>,
    lock: tokio::sync::Mutex<()>,
    records: parking_lot::Mutex<IndexMap<String, Record>>,
}

/// The Work sessions eve started, and the routes' logic. A cheap handle: clones share one registry.
#[derive(Clone)]
pub struct WorkSessions {
    inner: Arc<Inner>,
}

impl WorkSessions {
    pub fn new(opts: Options) -> Self {
        let root = match opts.root {
            Some(r) => py::normalize(&r),
            None => work_root(opts.environ.as_ref()),
        };
        let home = match opts.home {
            Some(h) => py::normalize(&h),
            None => config_dir(),
        };
        let run = opts.run;
        let live = match opts.live {
            Some(l) => l,
            None => {
                let run = run.clone();
                Arc::new(move || eve_claude_live::live_processes(&*run))
            }
        };
        let inner = Inner {
            registry: home.join(REGISTRY_NAME),
            files: files_dir(Some(&home)),
            root,
            home,
            state: opts.state,
            osascript: opts.osascript,
            run,
            kill: opts.kill,
            live,
            clock: opts.clock,
            launch: opts.launch,
            sleep: opts.sleep,
            environ: opts.environ,
            claude_dir: opts.claude_dir.map(|d| py::normalize(&d)),
            lock: tokio::sync::Mutex::new(()),
            records: parking_lot::Mutex::new(IndexMap::new()),
        };
        let this = WorkSessions { inner: Arc::new(inner) };
        this.load();
        this
    }

    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn home(&self) -> &Path {
        &self.inner.home
    }

    pub fn registry(&self) -> &Path {
        &self.inner.registry
    }

    pub fn files(&self) -> &Path {
        &self.inner.files
    }

    pub fn launch(&self) -> &Launch {
        &self.inner.launch
    }

    /// The records, by session id, in the order they were made (Python's `self.records`).
    pub fn records(&self) -> parking_lot::MutexGuard<'_, IndexMap<String, Record>> {
        self.inner.records.lock()
    }

    fn now(&self) -> f64 {
        (self.inner.clock)()
    }

    fn state(&self) -> Option<Arc<parking_lot::Mutex<State>>> {
        (self.inner.state)()
    }

    // -- the file --
    fn load(&self) {
        let raw = match std::fs::read(&self.inner.registry) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
            Err(e) => {
                tracing::warn!(
                    "work sessions: the registry could not be read ({}); starting empty",
                    py::os_error_name(&e)
                );
                return;
            }
        };
        let got: Value = match std::str::from_utf8(&raw) {
            Err(_) => {
                tracing::warn!("work sessions: the registry could not be read (UnicodeDecodeError); starting empty");
                return;
            }
            Ok(text) => match serde_json::from_str(text) {
                Ok(v) => v,
                Err(_) => {
                    tracing::warn!("work sessions: the registry could not be read (JSONDecodeError); starting empty");
                    return;
                }
            },
        };
        let rows = match got.get("sessions") {
            Some(Value::Array(rows)) => rows.clone(),
            _ => Vec::new(),
        };
        let mut records = self.inner.records.lock();
        for row in &rows {
            if let Some(rec) = Record::from_row(row) {
                records.insert(rec.sid.clone(), rec);
            }
        }
    }

    fn save_locked(&self, records: &mut IndexMap<String, Record>) {
        let now = self.now();
        records.retain(|_, r| !(r.over() && now - r.ended_at.max(r.created_at) > KEEP_SECS));
        let body = json!({"sessions": records.values().map(Record::saved).collect::<Vec<_>>()});
        let tmp = self.inner.registry.with_extension("tmp");
        let written = write_private(&tmp, &py::dumps(&body, Some(1)))
            .and_then(|()| std::fs::rename(&tmp, &self.inner.registry));
        if let Err(e) = written {
            tracing::warn!("work sessions: the registry could not be saved ({})", py::os_error_name(&e));
        }
    }

    fn save(&self) {
        let mut records = self.inner.records.lock();
        self.save_locked(&mut records);
    }

    // -- the hooks (the daemon calls this from its IPC handlers) --

    /// A hook event for any session; only Work sessions are recorded. Never fails.
    pub fn note(&self, evt: &str, req: &Value) {
        let Some(sid) = req.get("session_id").and_then(Value::as_str) else {
            return;
        };
        let mut records = self.inner.records.lock();
        let now = self.now();
        let Some(rec) = records.get_mut(sid) else {
            return;
        };
        if rec.over() {
            return;
        }
        rec.last_activity = now;
        let mut changed = false;
        match evt {
            "session_start" => {
                if let Some(path) = req.get("transcript_path").and_then(Value::as_str) {
                    if !path.is_empty() && path != rec.transcript_path {
                        rec.transcript_path = path.to_string();
                        changed = true;
                    }
                }
                if rec.phase == "starting" {
                    rec.phase = "idle".to_string();
                }
            }
            "turn_begin" => rec.phase = "working".to_string(),
            "turn_end" => rec.phase = "idle".to_string(),
            "notification" => {
                let kind = req.get("notification_type").and_then(Value::as_str);
                if eve_state::notification_waits(kind) {
                    rec.phase = "waiting".to_string();
                }
            }
            "pretooluse" => {
                if req.get("tool_name").and_then(Value::as_str) == Some("AskUserQuestion") {
                    rec.phase = "waiting".to_string();
                }
            }
            "posttooluse" => {
                if rec.phase == "waiting" {
                    rec.phase = "working".to_string();
                }
            }
            // Not the end of the process: /clear ends this session id and goes on under a new one, in the same
            // claude process (whose ps line still names this id). Whether it runs is ps's to say (list, reply).
            "session_end" => rec.phase = "idle".to_string(),
            _ => {}
        }
        if changed {
            self.save_locked(&mut records);
        }
    }

    /// Whether `token` is the eve MCP token of a Work session that has not ended (constant time).
    pub fn check_token(&self, sid: Option<&str>, token: Option<&str>) -> bool {
        let records = self.inner.records.lock();
        let Some(rec) = sid.and_then(|s| records.get(s)) else {
            return false;
        };
        let Some(token) = token.filter(|t| !t.is_empty()) else {
            return false;
        };
        if rec.over() || rec.token_hash.is_empty() {
            return false;
        }
        compare_digest(&hash_token(token), &rec.token_hash)
    }

    // -- helpers --
    fn transcript(&self, rec: Option<&Record>, session: Option<&KnownSession>) -> Option<PathBuf> {
        transcript_of(&self.inner.root, self.inner.claude_dir.as_deref(), rec, session)
    }

    /// End a record (its process is gone, or it was stopped) and delete its files; the record as it now is.
    fn end(&self, sid: &str) -> Option<Record> {
        let rec = {
            let mut records = self.inner.records.lock();
            let now = self.now();
            let mut changed = false;
            if let Some(rec) = records.get_mut(sid) {
                if !rec.over() {
                    rec.ended_at = now;
                    changed = true;
                }
            }
            let rec = records.get(sid).cloned();
            if changed {
                self.save_locked(&mut records);
            }
            rec
        };
        self.unlink_files(sid);
        rec
    }

    fn unlink_files(&self, sid: &str) {
        let _ = std::fs::remove_file(self.inner.files.join(format!("{sid}.mcp.json")));
        let _ = std::fs::remove_file(self.inner.files.join(format!("{sid}.task")));
    }

    /// One Work session's row. `pids` None is a blind probe: the session counts as alive.
    pub fn row(&self, rec: &Record, pids: Option<&IndexMap<String, i64>>, path: Option<&Path>, line: &str) -> Value {
        row_of(&self.inner.root, self.now(), rec, pids, path, line)
    }

    // -- the routes: each answers (HTTP status, JSON body) --

    /// `GET /api/link/sessions`.
    pub async fn list(&self) -> Answer {
        let sessions: Vec<KnownSession> = match self.state() {
            Some(state) => {
                let st = state.lock();
                st.sessions.values().map(|s| KnownSession::of(&st, s)).collect()
            }
            None => Vec::new(),
        };
        let (run, live) = (self.inner.run.clone(), self.inner.live.clone());
        let (pids, live) = to_thread(move || (session_pids(&*run), live())).await;
        let records: Vec<Record> = self.records().values().cloned().collect();
        let inner = self.inner.clone();
        let (rows, gone) = to_thread(move || {
            rows_of(&inner.root, inner.claude_dir.as_deref(), (inner.clock)(), &records, &sessions, pids.as_ref(), live.as_ref())
        })
        .await;
        for sid in gone {
            tracing::info!("work sessions: {} is gone (no claude process)", sid8(&sid));
            self.end(&sid);
        }
        (200, json!({"ok": true, "folder": py::name(&self.inner.root), "sessions": rows}))
    }

    /// `POST /api/link/sessions/start`.
    pub async fn start(&self, task: &Value, client_id: &Value) -> Answer {
        let task = match task {
            Value::String(t) if !py::strip(t).is_empty() => py::strip(t).to_string(),
            _ => return refuse(400, "say what Claude should work on"),
        };
        if py::len(&task) > MAX_TASK_CHARS {
            return refuse(400, &format!("at most {MAX_TASK_CHARS} characters"));
        }
        let client_id = match client_id {
            Value::Null => String::new(),
            Value::String(c) if py::len(c) <= MAX_CLIENT_ID_CHARS => c.clone(),
            _ => return refuse(400, "client_id must be a string of at most 64 characters"),
        };
        let _guard = self.inner.lock.lock().await;
        let now = self.now();
        if !client_id.is_empty() {
            let found = self
                .records()
                .values()
                .find(|r| r.client_id == client_id && now - r.created_at < DEDUPE_SECS)
                .cloned();
            if let Some(rec) = found {
                let path = self.transcript(Some(&rec), None);
                return (200, json!({"ok": true, "session": self.row(&rec, None, path.as_deref(), "")}));
            }
        }
        if let Err(e) = std::fs::create_dir_all(&self.inner.root).and_then(|()| {
            if self.inner.root.is_dir() {
                Ok(())
            } else {
                Err(std::io::Error::from_raw_os_error(libc::EEXIST))
            }
        }) {
            return refuse(503, &format!("the work folder is not there ({})", py::os_error_name(&e)));
        }
        let sid = uuid::Uuid::new_v4().to_string();
        let token = token_urlsafe();
        let first = py::splitlines(&task).first().copied().unwrap_or("");
        let mut rec = Record::new(sid.clone(), now, flat(first, TITLE_CHARS), hash_token(&token));
        rec.client_id = client_id;
        rec.last_activity = now;
        let config = self.inner.launch.mcp_config(&sid, &token, self.inner.environ.as_ref());
        let written = write_private(&self.inner.files.join(format!("{sid}.task")), &task)
            .and_then(|()| write_private(&self.inner.files.join(format!("{sid}.mcp.json")), &py::dumps(&config, None)));
        if let Err(e) = written {
            tracing::warn!("work sessions: could not write the session's files ({})", py::os_error_name(&e));
            return refuse(503, "eve couldn't write the session's files");
        }
        {
            let mut records = self.inner.records.lock();
            records.insert(sid.clone(), rec.clone());
            self.save_locked(&mut records);
        }
        let rec_made = rec;
        let line = self.inner.launch.line(&self.inner.root, &sid);
        let (code, out) = match (self.inner.osascript)(TERMINAL_OPEN.to_string(), vec![line]).await {
            Ok(answer) => answer,
            Err(e) => (1, py::os_error_name(&e).to_string()),
        };
        if code != 0 {
            tracing::warn!("work sessions: Terminal did not open (exit {code})");
            {
                let mut records = self.inner.records.lock();
                records.shift_remove(&sid);
                self.save_locked(&mut records);
            }
            self.unlink_files(&sid);
            return refuse(503, "eve couldn't open Terminal on the Mac");
        }
        let out = py::strip(&out);
        let (wid, tty) = out.split_once(' ').unwrap_or((out, ""));
        // the window opened; without its id or tty the session runs, and replies say they can't reach it
        let window_id = if !wid.is_empty() && wid.chars().all(|c| c.is_ascii_digit()) {
            wid.parse::<i64>().ok().filter(|w| *w > 0)
        } else {
            None
        };
        let tty = py::strip(tty);
        let tty = if tty.starts_with("/dev/") { tty.to_string() } else { String::new() };
        // Python sets these on its own `rec` object, so the answer carries them whatever happened to the dict.
        let mut rec = self.inner.records.lock().get(&sid).cloned().unwrap_or(rec_made);
        rec.window_id = window_id;
        rec.tty = tty;
        {
            let mut records = self.inner.records.lock();
            if let Some(r) = records.get_mut(&sid) {
                r.window_id = rec.window_id;
                r.tty = rec.tty.clone();
            }
            self.save_locked(&mut records);
        }
        tracing::info!("work sessions: {} started (task {} characters)", sid8(&sid), py::len(&task));
        (200, json!({"ok": true, "session": self.row(&rec, None, None, "")}))
    }

    /// `GET /api/link/sessions/<sid>`.
    pub async fn detail(&self, sid: &str) -> Answer {
        let rec = self.records().get(sid).cloned();
        let state = self.state();
        let session = state.as_ref().and_then(|st| {
            let st = st.lock();
            st.sessions.get(sid).map(|s| KnownSession::of(&st, s))
        });
        if rec.is_none() && session.is_none() {
            return refuse(404, UNKNOWN);
        }
        let path = self.transcript(rec.as_ref(), session.as_ref());
        let read_path = path.clone();
        let (turns, cut) = to_thread(move || transcript_turns(read_path.as_deref())).await;
        let mut ended = false;
        let mut row = if let Some(mut rec) = rec {
            let run = self.inner.run.clone();
            let pids = to_thread(move || session_pids(&*run)).await;
            if !rec.over()
                && pids.as_ref().is_some_and(|p| !p.contains_key(sid))
                && self.now() - rec.created_at >= GRACE_SECS
            {
                tracing::info!("work sessions: {} is gone (no claude process)", sid8(&rec.sid));
                // claude exited (/exit, a crash): the sheet shows it ended
                rec = self.end(sid).unwrap_or_else(|| {
                    let mut r = rec.clone();
                    r.ended_at = self.now();
                    r
                });
            }
            let mut row = self.row(&rec, pids.as_ref(), path.as_deref(), "");
            if rec.over() {
                ended = true;
                row["state"] = Value::from("ended");
                row["can_reply"] = Value::from(false);
                row["can_stop"] = Value::from(false);
            }
            row
        } else {
            let session = session.expect("a known session when there is no record");
            let name = py::name(Path::new(session.cwd.as_deref().unwrap_or("")));
            json!({"id": sid, "folder": name, "title": name, "state": session.state(),
                   "last_activity": session.started_at.max(mtime(path.as_deref())) as i64,
                   "from_work": false, "can_reply": false, "can_stop": false})
        };
        let last = turns.iter().rev().find(|t| t.role == "claude").map(|t| t.text.as_str()).unwrap_or("");
        row["last_line"] = Value::from(flat(last, LINE_CHARS));
        let turns: Vec<Value> = turns.iter().map(|t| json!({"role": t.role, "text": t.text, "at": t.at})).collect();
        (200, json!({"ok": true, "session": row, "turns": turns, "cut": cut, "ended": ended}))
    }

    fn not_ours_or_unknown(&self, sid: &str) -> Answer {
        let known = self.state().is_some_and(|st| st.lock().sessions.contains_key(sid));
        if known {
            refuse(409, NOT_OURS)
        } else {
            refuse(404, UNKNOWN)
        }
    }

    /// `POST /api/link/sessions/<sid>/reply`.
    pub async fn reply(&self, sid: &str, text: &Value) -> Answer {
        let Some(rec) = self.records().get(sid).cloned() else {
            return self.not_ours_or_unknown(sid);
        };
        let text = match text {
            Value::String(t) if !py::strip(t).is_empty() => t,
            _ => return refuse(400, "nothing to send"),
        };
        // One line: a newline inside would submit early. The owner's words, never a script's text.
        let line = py::join_words(text);
        if py::len(&line) > MAX_REPLY_CHARS {
            return refuse(400, &format!("at most {MAX_REPLY_CHARS} characters"));
        }
        let ended = || (409, json!({"ok": false, "reason": ENDED, "code": ENDED_CODE}));
        if rec.over() {
            return ended();
        }
        let run = self.inner.run.clone();
        let Some(pids) = to_thread(move || session_pids(&*run)).await else {
            return refuse(409, NO_WINDOW);
        };
        if !pids.contains_key(sid) {
            if self.now() - rec.created_at >= GRACE_SECS {
                self.end(sid);
                return ended();
            }
            return refuse(409, "that session is still starting; try again in a moment");
        }
        let (Some(window_id), false) = (rec.window_id, rec.tty.is_empty()) else {
            return refuse(409, NO_WINDOW);
        };
        let args = vec![window_id.to_string(), rec.tty.clone(), line.clone()];
        let (code, out) = (self.inner.osascript)(TERMINAL_TYPE.to_string(), args).await.unwrap_or((1, String::new()));
        if code != 0 || out != "ok" {
            let why = if out == "gone" || out == "idle" { out.as_str() } else { "error" };
            tracing::info!("work sessions: reply to {} not typed ({why})", sid8(sid));
            return refuse(409, NO_WINDOW);
        }
        let now = self.now();
        if let Some(r) = self.records().get_mut(sid) {
            r.last_activity = now;
        }
        tracing::info!("work sessions: reply typed into {} ({} characters)", sid8(sid), py::len(&line));
        (200, json!({"ok": true}))
    }

    /// `POST /api/link/sessions/<sid>/stop`. An `Err` is an `os.kill` failure other than "no such process" or
    /// "not permitted" (which Python lets propagate too).
    pub async fn stop(&self, sid: &str) -> std::io::Result<Answer> {
        if !self.records().contains_key(sid) {
            return Ok(self.not_ours_or_unknown(sid));
        }
        let kill = |pid: i64, sig: i32| -> std::io::Result<()> {
            match (self.inner.kill)(pid, sig) {
                Err(e) if matches!(e.raw_os_error(), Some(libc::ESRCH | libc::EPERM | libc::EACCES))
                    || matches!(e.kind(), std::io::ErrorKind::PermissionDenied) =>
                {
                    Ok(())
                }
                other => other,
            }
        };
        let run = self.inner.run.clone();
        let pids = to_thread(move || session_pids(&*run)).await;
        let pid = pids.as_ref().and_then(|p| p.get(sid).copied());
        if let Some(pid) = pid {
            kill(pid, libc::SIGTERM)?;
            let mut waited = 0.0;
            let mut gone = false;
            while waited < STOP_WAIT_SECS {
                (self.inner.sleep)(0.25).await;
                waited += 0.25;
                let run = self.inner.run.clone();
                let now_pids = to_thread(move || session_pids(&*run)).await;
                if now_pids.is_some_and(|p| !p.contains_key(sid)) {
                    gone = true;
                    break;
                }
            }
            if !gone {
                kill(pid, libc::SIGKILL)?;
            }
        }
        if let Some(r) = self.records().get_mut(sid) {
            r.stopped = true;
        }
        self.end(sid);
        self.save();
        if let Some(state) = self.state() {
            state.lock().session_end(sid);
        }
        let how = if pid.is_some() { "signalled" } else { "already gone" };
        tracing::info!("work sessions: {} stopped ({how})", sid8(sid));
        Ok((200, json!({"ok": true, "stopped": true})))
    }
}

fn transcript_of(
    root: &Path,
    claude_dir: Option<&Path>,
    rec: Option<&Record>,
    session: Option<&KnownSession>,
) -> Option<PathBuf> {
    if let Some(rec) = rec.filter(|r| !r.transcript_path.is_empty()) {
        return Some(PathBuf::from(&rec.transcript_path));
    }
    if let Some(path) = session.and_then(|s| s.transcript_path.as_deref()).filter(|p| !p.is_empty()) {
        return Some(PathBuf::from(path));
    }
    let rec = rec?;
    match derived_transcript(root, &rec.sid, claude_dir) {
        Ok(p) => Some(p),
        Err(e) => {
            tracing::debug!("work sessions: no transcript path ({e})");
            None
        }
    }
}

fn row_of(
    root: &Path,
    now: f64,
    rec: &Record,
    pids: Option<&IndexMap<String, i64>>,
    path: Option<&Path>,
    line: &str,
) -> Value {
    let alive = pids.is_none_or(|p| p.contains_key(&rec.sid));
    let mut state = if STATES.contains(&rec.phase.as_str()) { rec.phase.as_str() } else { "idle" };
    if pids.is_some_and(|p| !p.contains_key(&rec.sid)) && now - rec.created_at < GRACE_SECS {
        state = "starting"; // within its grace: opened, claude not up yet
    }
    json!({"id": rec.sid, "folder": py::name(root), "title": rec.title, "state": state,
           "last_activity": rec.last_activity.max(rec.created_at).max(mtime(path)) as i64,
           "last_line": line, "from_work": true,
           "can_reply": alive && rec.window_id.is_some() && !rec.tty.is_empty(), "can_stop": true})
}

/// (the rows, Work records whose process is gone). Runs on a thread (it reads transcripts), so it gets snapshots
/// of the records and the daemon's sessions, taken on the loop.
fn rows_of(
    root: &Path,
    claude_dir: Option<&Path>,
    now: f64,
    records: &[Record],
    sessions: &[KnownSession],
    pids: Option<&IndexMap<String, i64>>,
    live: Option<&IndexMap<String, bool>>,
) -> (Vec<Value>, Vec<String>) {
    let mut rows: Vec<Value> = Vec::new();
    let mut gone: Vec<String> = Vec::new();
    for rec in records {
        if rec.over() {
            continue;
        }
        if pids.is_some_and(|p| !p.contains_key(&rec.sid)) && now - rec.created_at >= GRACE_SECS {
            gone.push(rec.sid.clone());
            continue;
        }
        let path = transcript_of(root, claude_dir, Some(rec), None);
        rows.push(row_of(root, now, rec, pids, path.as_deref(), &last_line(path.as_deref())));
    }
    for s in sessions {
        let Some(cwd) = s.cwd.as_deref().filter(|c| !c.is_empty()) else {
            continue;
        };
        if records.iter().any(|r| r.sid == s.session_id) {
            continue;
        }
        if let Some(live) = live {
            if !eve_claude_live::alive(cwd, live.keys()) && now - s.started_at >= GRACE_SECS {
                continue;
            }
        }
        let path = transcript_of(root, claude_dir, None, Some(s));
        let name = py::name(Path::new(cwd));
        rows.push(json!({"id": s.session_id, "folder": name, "title": name, "state": s.state(),
                         "last_activity": s.started_at.max(mtime(path.as_deref())) as i64,
                         "last_line": last_line(path.as_deref()),
                         "from_work": false, "can_reply": false, "can_stop": false}));
    }
    rows.sort_by(|a, b| b["last_activity"].as_i64().cmp(&a["last_activity"].as_i64()));
    rows.truncate(MAX_LISTED);
    (rows, gone)
}

// ---- the session's own first moment: eve-work-sessions launch <sid> -----------------------------------------

/// `os.execvp(file, args)`; returns only when it failed.
pub fn execvp(file: &str, args: &[String]) -> std::io::Error {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        std::process::Command::new(file).args(args.iter().skip(1)).exec()
    }
    #[cfg(not(unix))]
    {
        let _ = (file, args);
        std::io::Error::new(std::io::ErrorKind::Unsupported, "exec is POSIX-only")
    }
}

/// `main()` over the process: `launch <sid>` from the command line, `~/.config/eve-bridge`, the real PATH and exec.
pub fn main() -> i32 {
    // sys.argv never fails to decode (surrogateescape); a word that is not UTF-8 is no session id either way.
    let args: Vec<String> = std::env::args_os().skip(1).map(|a| a.to_string_lossy().into_owned()).collect();
    let which = |name: &str| which::which(name).ok();
    main_with(&args, None, &which, &mut |file, argv| Err(execvp(file, argv)))
}

/// Read and delete the session's task, then become Claude Code ([`launch_argv`]). Prints nothing of the task.
/// `execvp` returns only where it failed (or in a test's stand-in, as `Ok`).
pub fn main_with(
    args: &[String],
    home: Option<&Path>,
    which: &dyn Fn(&str) -> Option<PathBuf>,
    execvp: &mut dyn FnMut(&str, &[String]) -> std::io::Result<()>,
) -> i32 {
    if args.len() != 2 || args[0] != "launch" || !is_uuid(&args[1]) {
        eprintln!("usage: eve-work-sessions launch <session id>");
        return 2;
    }
    let sid = &args[1];
    let folder = files_dir(home);
    let task_path = folder.join(format!("{sid}.task"));
    let task = match std::fs::read(&task_path) {
        Ok(raw) => raw,
        Err(_) => {
            eprintln!("This Work session's task is gone (already started, or never written).");
            return 1;
        }
    };
    let task = match String::from_utf8(task) {
        // `read_text` reads in universal-newlines mode: "\r\n" and a lone "\r" both arrive as "\n".
        Ok(t) => t.replace("\r\n", "\n").replace('\r', "\n"),
        Err(_) => {
            eprintln!("UnicodeDecodeError: this Work session's task is not UTF-8.");
            return 1;
        }
    };
    let _ = std::fs::remove_file(&task_path);
    let Some(claude) = which("claude") else {
        eprintln!("Claude Code (claude) is not on this shell's PATH.");
        return 127;
    };
    let claude = claude.to_string_lossy().into_owned();
    let argv = match launch_argv(sid, &task, &folder.join(format!("{sid}.mcp.json")), &claude) {
        Ok(argv) => argv,
        Err(e) => {
            eprintln!("ValueError: {e}");
            return 1;
        }
    };
    match execvp(&argv[0], &argv) {
        Ok(()) => 0, // only reached when execvp is a test's stand-in
        Err(e) => {
            eprintln!("{}: {e}", py::os_error_name(&e));
            1
        }
    }
}

#[cfg(test)]
mod tests;
