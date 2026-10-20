//! PostToolUse hook — fire-and-forget notice that a tool call finished.

use std::io::Write;

use serde_json::json;

use crate::{client, payload_str, HookError, Poster};

/// `main()` with stdin and the daemon client passed in.
pub fn run(raw: &[u8], post: &mut Poster<'_>, _out: &mut dyn Write) -> Result<i32, HookError> {
    let payload = client::read_hook_input_from(raw)?;
    post(
        json!({
            "evt": "posttooluse",
            "session_id": payload_str(&payload, "session_id")?,
            "tool_use_id": payload_str(&payload, "tool_use_id")?,
            "tool_name": payload_str(&payload, "tool_name")?,
            // No result tail: the phone relay (telegram.py, "claude on") forwards only what the terminal shows
            // in white, never a tool's output (owner, 2026-09-21).
        }),
        client::DEFAULT_TIMEOUT_SECS,
    );
    Ok(0)
}

/// `python -m eve_bridge.hooks.posttooluse`.
pub fn main() -> i32 {
    crate::run_main(run)
}
