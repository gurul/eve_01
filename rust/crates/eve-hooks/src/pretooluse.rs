//! PreToolUse hook — blocks Claude Code's tool call until the stick's button decides.
//!
//! If the daemon is unreachable or BLE is not connected, emits no decision so that
//! Claude Code's normal approval flow runs.
//!
//! stdin: `{ session_id, tool_name, tool_input, tool_use_id, ... }`
//! stdout (on decision): `{ "hookSpecificOutput": { "hookEventName": "PreToolUse",
//!                                                  "permissionDecision": "allow"|"deny"|"ask",
//!                                                  "permissionDecisionReason": "..." } }`

use std::io::Write;

use serde_json::{json, Map, Value};

use crate::{client, payload_get, payload_str, py_str, py_strip, py_truthy, write_json_line, HookError, Poster};

/// Hard upper bound for how long this hook blocks. Must be < the `timeout` we
/// set in settings.json (330 s) and > the daemon's permission wait
/// (telegram.DEFAULT_PERMISSION_TIMEOUT_SECS, 240 s). 5 minutes is plenty
/// of human reaction time and still leaves headroom.
pub const BLOCK_TIMEOUT_SECS: f64 = 320.0;

/// The values iterating `v` yields in Python, for the shapes that matter here:
/// a list its items, a str its characters and a dict its keys (none of them a
/// dict, so the caller skips them all). A number or bool is not iterable.
fn py_iter(v: &Value) -> Result<Vec<Value>, HookError> {
    match v {
        Value::Array(items) => Ok(items.clone()),
        Value::String(s) => Ok(s.chars().map(|c| Value::String(c.to_string())).collect()),
        Value::Object(m) => Ok(m.keys().map(|k| Value::String(k.clone())).collect()),
        Value::Null => Ok(Vec::new()),
        other => Err(HookError::not_iterable(other)),
    }
}

/// `str(d.get(key) or "").strip()`.
fn text_of(d: &Map<String, Value>, key: &str) -> String {
    match d.get(key) {
        Some(v) if py_truthy(v) => py_strip(&py_str(v)).to_string(),
        _ => String::new(),
    }
}

/// An AskUserQuestion call as one line the owner can answer from the phone: each question with its
/// options numbered in parentheses, so a reply of "2" picks the second (the relay types it into the
/// dialog). Empty when the input is not that shape.
pub fn question(tool_input: &Map<String, Value>) -> Result<String, HookError> {
    let Some(Value::Array(questions)) = tool_input.get("questions") else {
        return Ok(String::new());
    };
    let mut lines = Vec::new();
    for q in questions {
        let Value::Object(q) = q else { continue };
        let text = text_of(q, "question");
        let options = match q.get("options") {
            Some(v) if py_truthy(v) => py_iter(v)?,
            _ => Vec::new(),
        };
        let labels: Vec<String> = options
            .iter()
            .filter_map(|o| o.as_object().map(|o| text_of(o, "label")))
            .filter(|x| !x.is_empty())
            .collect();
        if !text.is_empty() {
            let mut line = text;
            if !labels.is_empty() {
                let numbered: Vec<String> = labels.iter().enumerate().map(|(i, x)| format!("{}. {x}", i + 1)).collect();
                line.push_str(" (");
                line.push_str(&numbered.join(" / "));
                line.push(')');
            }
            lines.push(line);
        }
    }
    Ok(lines.join(" | "))
}

/// Short human-readable hint from a tool_input dict (`_summarize`).
pub fn summarize(tool_input: &Value) -> Result<String, HookError> {
    match tool_input {
        Value::Object(map) => {
            let asked = question(map)?;
            if !asked.is_empty() {
                return Ok(asked);
            }
            // Bash: command; Edit/Write: file_path; fallback: first string value.
            for key in ["command", "file_path", "path", "url"] {
                if let Some(Value::String(v)) = map.get(key) {
                    if !v.is_empty() {
                        return Ok(v.clone());
                    }
                }
            }
            for v in map.values() {
                if let Value::String(v) = v {
                    if !v.is_empty() {
                        return Ok(v.clone());
                    }
                }
            }
            Ok(String::new())
        }
        Value::String(s) => Ok(s.clone()),
        _ => Ok(String::new()),
    }
}

/// Human-readable reason surfaced to the model alongside the decision.
pub fn decision_reason(decision: &str) -> &'static str {
    if decision == "allow" {
        return "User approved on Hardware Eve device.";
    }
    if decision == "deny" {
        return "User denied on Hardware Eve device. \
                Do not retry with alternative phrasings; the human has rejected this action.";
    }
    // "ask" — daemon explicitly bounced this to Claude Code's normal flow.
    "Hardware Eve deferred to Claude Code's normal approval flow."
}

/// `main()` with stdin, the daemon client and stdout passed in.
pub fn run(raw: &[u8], post: &mut Poster<'_>, out: &mut dyn Write) -> Result<i32, HookError> {
    let payload = client::read_hook_input_from(raw)?;
    let event = json!({
        "evt": "pretooluse",
        "session_id": payload_str(&payload, "session_id")?,
        "tool_use_id": payload_str(&payload, "tool_use_id")?,
        "tool_name": payload_str(&payload, "tool_name")?,
        "hint": summarize(&payload_get(&payload, "tool_input", Value::Null)?)?,
        "cwd": payload_str(&payload, "cwd")?,
        // Claude Code's permission mode for this session ("default", "acceptEdits", "plan",
        // "bypassPermissions"): a phone that is asked in bypass mode is asking for nothing.
        "permission_mode": payload_str(&payload, "permission_mode")?,
    });
    let resp = match post(event, BLOCK_TIMEOUT_SECS) {
        None | Some(Value::Null) => return Ok(0),
        Some(r) => r,
    };
    if !py_truthy(&payload_get(&resp, "ok", Value::Null)?) {
        // Daemon unreachable or errored — defer to Claude Code's default behavior.
        return Ok(0);
    }
    let decision = match payload_get(&resp, "decision", Value::Null)? {
        Value::String(d) if matches!(d.as_str(), "allow" | "deny" | "ask") => d,
        _ => return Ok(0),
    };
    // Always surface a permissionDecisionReason so the model knows the
    // decision came from a human at the Hardware Eve device. Without
    // this, Claude Code treats a bare `permissionDecision: "deny"` as a
    // generic hook block and may attempt alternative phrasings to work
    // around it (e.g. dropping `-rf`, splitting an `rm -rf foo/` into
    // per-file `rm`s). A clear reason short-circuits that.
    let reason = decision_reason(&decision);
    let out_obj = json!({
        "hookSpecificOutput": {
            "hookEventName": "PreToolUse",
            "permissionDecision": decision,
            "permissionDecisionReason": reason,
        }
    });
    write_json_line(out, &out_obj)?;
    Ok(0)
}

/// `python -m eve_bridge.hooks.pretooluse`.
pub fn main() -> i32 {
    crate::run_main(run)
}
