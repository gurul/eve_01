//! `python -m eve_bridge.watch --render URL [TIMEOUT]`: the watcher's render child (watch::render::render_main).

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--render") {
        std::process::exit(eve_web::watch::render::render_main(&args));
    }
}
