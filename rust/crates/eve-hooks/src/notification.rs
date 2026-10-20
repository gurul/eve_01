//! Notification hook — Claude needs something from the user.
//!
//! Claude Code fires Notification when it needs permission for a tool call or
//! has been waiting on user input. Forward it so the eve goes into its
//! attention animation until the user responds (next UserPromptSubmit /
//! PostToolUse clears it).

use std::io::Write;

use serde_json::{json, Value};

use crate::{client, payload_get, payload_str, py_truthy, HookError, Poster};

/// `payload.get("type") or payload.get("notification_type") or ""`.
fn notification_type(payload: &Value) -> Result<Value, HookError> {
    let t = payload_get(payload, "type", Value::Null)?;
    if py_truthy(&t) {
        return Ok(t);
    }
    let nt = payload_get(payload, "notification_type", Value::Null)?;
    if py_truthy(&nt) {
        return Ok(nt);
    }
    Ok(Value::String(String::new()))
}

/// `main()` with stdin and the daemon client passed in.
pub fn run(raw: &[u8], post: &mut Poster<'_>, _out: &mut dyn Write) -> Result<i32, HookError> {
    let payload = client::read_hook_input_from(raw)?;
    post(
        json!({
            "evt": "notification",
            "session_id": payload_str(&payload, "session_id")?,
            "message": payload_str(&payload, "message")?,
            "notification_type": notification_type(&payload)?,
        }),
        client::DEFAULT_TIMEOUT_SECS,
    );
    Ok(0)
}

/// `python -m eve_bridge.hooks.notification`.
pub fn main() -> i32 {
    crate::run_main(run)
}
