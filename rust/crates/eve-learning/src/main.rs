//! `python -m eve_bridge.learning` (learning/__main__.py): `raise SystemExit(main())`.

fn main() {
    // Python's logging is left unconfigured here, so warnings and errors reach stderr (logging.lastResort).
    tracing_subscriber::fmt().with_max_level(tracing::Level::WARN).with_writer(std::io::stderr).init();
    match eve_learning::server::main() {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{e:#}");
            std::process::exit(1);
        }
    }
}
