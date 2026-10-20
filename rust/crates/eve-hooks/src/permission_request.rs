//! PermissionRequest hook: a permission dialog Claude Code is about to show, answered from the phone.
//!
//! PreToolUse only sees the tools it is registered for (Bash, Read), so a dialog for Edit, Write, WebFetch,
//! an MCP tool … reached the phone as nothing but "Claude is waiting on you" (owner, 2026-09-23). This hook
//! fires for every dialog. With the Telegram relay on ("claude on") the daemon asks the owner yes/no in the
//! chat; any other state, no answer, or an unreachable daemon returns nothing, and the dialog on the Mac
//! runs exactly as before.
//!
//! stdin: `{ session_id, tool_name, tool_input, tool_use_id, cwd, permission_mode, ... }`
//! stdout (on decision): `{ "hookSpecificOutput": { "hookEventName": "PermissionRequest",
//!                                                  "decision": {"behavior": "allow"} } }`
//!     or `{"behavior": "deny", "message": "..."}` — the shape Claude Code 2.1.280 validates.

use std::io::Write;

use serde_json::{json, Value};

use crate::pretooluse::{summarize, BLOCK_TIMEOUT_SECS};
use crate::{client, payload_get, payload_str, py_truthy, write_json_line, HookError, Poster};

pub const DENY_MESSAGE: &str = "The owner denied this from their phone. Do not retry it with alternative phrasings; \
                                the human has rejected this action.";

/// `main()` with stdin, the daemon client and stdout passed in.
pub fn run(raw: &[u8], post: &mut Poster<'_>, out: &mut dyn Write) -> Result<i32, HookError> {
    let payload = client::read_hook_input_from(raw);
    let event = json!({
        "evt": "permissionrequest",
        "session_id": payload_str(&payload, "session_id")?,
        "tool_use_id": payload_str(&payload, "tool_use_id")?,
        "tool_name": payload_str(&payload, "tool_name")?,
        "hint": summarize(&payload_get(&payload, "tool_input", Value::Null)?)?,
        "cwd": payload_str(&payload, "cwd")?,
        "permission_mode": payload_str(&payload, "permission_mode")?,
    });
    let resp = match post(event, BLOCK_TIMEOUT_SECS) {
        None | Some(Value::Null) => return Ok(0),
        Some(r) => r,
    };
    if !py_truthy(&payload_get(&resp, "ok", Value::Null)?) {
        return Ok(0);
    }
    let behavior = match payload_get(&resp, "decision", Value::Null)?.as_str() {
        Some("allow") => json!({"behavior": "allow"}),
        Some("deny") => json!({"behavior": "deny", "message": DENY_MESSAGE}),
        _ => return Ok(0),
    };
    write_json_line(out, &json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest",
                                                        "decision": behavior}}))?;
    Ok(0)
}

/// `python -m eve_bridge.hooks.permission_request`.
pub fn main() -> i32 {
    crate::run_main(run)
}
