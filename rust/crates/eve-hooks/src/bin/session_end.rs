//! `python -m eve_bridge.hooks.session_end` as a binary.

fn main() {
    std::process::exit(eve_hooks::session_end::main());
}
