//! UserPromptSubmit hook — marks the session as running and surfaces the prompt.

use std::io::Write;

use serde_json::{json, Value};

use crate::{client, payload_get, payload_str, py_strip, py_truthy, HookError, Poster};

/// `(payload.get("prompt") or "").strip()`: a truthy non-string has no `strip`.
fn prompt_of(payload: &Value) -> Result<String, HookError> {
    match payload_get(payload, "prompt", Value::Null)? {
        Value::String(s) => Ok(py_strip(&s).to_string()),
        v if py_truthy(&v) => Err(HookError::no_attribute(&v, "strip")),
        _ => Ok(String::new()),
    }
}

/// `main()` with stdin and the daemon client passed in.
pub fn run(raw: &[u8], post: &mut Poster<'_>, _out: &mut dyn Write) -> Result<i32, HookError> {
    let payload = client::read_hook_input_from(raw);
    post(
        json!({
            "evt": "turn_begin",
            "session_id": payload_str(&payload, "session_id")?,
            "prompt": prompt_of(&payload)?,
        }),
        client::DEFAULT_TIMEOUT_SECS,
    );
    Ok(0)
}

/// `python -m eve_bridge.hooks.user_prompt_submit`.
pub fn main() -> i32 {
    crate::run_main(run)
}
