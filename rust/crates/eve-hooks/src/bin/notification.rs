//! `python -m eve_bridge.hooks.notification` as a binary.

fn main() {
    std::process::exit(eve_hooks::notification::main());
}
