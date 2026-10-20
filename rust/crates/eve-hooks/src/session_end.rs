//! SessionEnd hook — tell the daemon this session is gone.

use std::io::Write;

use serde_json::json;

use crate::{client, payload_str, HookError, Poster};

/// `main()` with stdin and the daemon client passed in.
pub fn run(raw: &[u8], post: &mut Poster<'_>, _out: &mut dyn Write) -> Result<i32, HookError> {
    let payload = client::read_hook_input_from(raw);
    post(
        json!({
            "evt": "session_end",
            "session_id": payload_str(&payload, "session_id")?,
        }),
        client::DEFAULT_TIMEOUT_SECS,
    );
    Ok(0)
}

/// `python -m eve_bridge.hooks.session_end`.
pub fn main() -> i32 {
    crate::run_main(run)
}
