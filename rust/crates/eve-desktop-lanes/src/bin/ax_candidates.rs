//! `python -m eve_bridge.ax_candidates`: walk one app's focused window and print its candidates.

fn main() {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    std::process::exit(eve_desktop_lanes::ax_candidates::main(&argv));
}
