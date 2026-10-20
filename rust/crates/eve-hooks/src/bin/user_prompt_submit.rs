//! `python -m eve_bridge.hooks.user_prompt_submit` as a binary.

fn main() {
    std::process::exit(eve_hooks::user_prompt_submit::main());
}
