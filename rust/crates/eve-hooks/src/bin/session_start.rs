//! `python -m eve_bridge.hooks.session_start` as a binary.

fn main() {
    std::process::exit(eve_hooks::session_start::main());
}
