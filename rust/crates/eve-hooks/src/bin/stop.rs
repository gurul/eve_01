//! `python -m eve_bridge.hooks.stop` as a binary.

fn main() {
    std::process::exit(eve_hooks::stop::main());
}
