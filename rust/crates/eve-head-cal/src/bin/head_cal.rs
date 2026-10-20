//! `python -m eve_bridge.head_cal`: the head calibration page on the default port, opened in the browser.

fn main() {
    match eve_head_cal::serve(8766, None, true) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
