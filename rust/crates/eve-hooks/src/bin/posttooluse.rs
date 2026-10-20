//! `python -m eve_bridge.hooks.posttooluse` as a binary.

fn main() {
    std::process::exit(eve_hooks::posttooluse::main());
}
