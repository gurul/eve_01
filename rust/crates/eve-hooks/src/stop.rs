//! Stop hook — turn finished, clear running flag.

use std::io::Write;

use serde_json::json;

use crate::{client, payload_str, HookError, Poster};

/// `main()` with stdin and the daemon client passed in.
pub fn run(raw: &[u8], post: &mut Poster<'_>, _out: &mut dyn Write) -> Result<i32, HookError> {
    let payload = client::read_hook_input_from(raw)?;
    post(
        json!({
            "evt": "turn_end",
            "session_id": payload_str(&payload, "session_id")?,
            "transcript_path": payload_str(&payload, "transcript_path")?,
        }),
        client::DEFAULT_TIMEOUT_SECS,
    );
    Ok(0)
}

/// `python -m eve_bridge.hooks.stop`.
pub fn main() -> i32 {
    crate::run_main(run)
}
