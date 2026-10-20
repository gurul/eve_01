//! `eve-bridge` (and `python -m eve_bridge`, __main__.py): `sys.exit(main())`.

fn main() {
    std::process::exit(eve_bridge::cli::main(None));
}
