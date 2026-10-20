//! `python -m eve_bridge.hooks.permission_request` as a binary.

fn main() {
    std::process::exit(eve_hooks::permission_request::main());
}
