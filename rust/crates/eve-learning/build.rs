//! Embeds `bridge/src/eve_bridge/learning/web/` (the page, its fonts and the tldraw whiteboard bundle) so the Rust
//! server serves the same bytes the Python one reads from disk. Writes `$OUT_DIR/web_assets.rs`: a sorted table of
//! (path relative to web/, bytes) built from `include_bytes!`.

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, base, out);
        } else if path.is_file() {
            let rel = path.strip_prefix(base).expect("under web/");
            let name = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
            out.push((name, path.clone()));
        }
    }
}

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let web = manifest.join("../../../bridge/src/eve_bridge/learning/web");
    let web = fs::canonicalize(&web).unwrap_or(web);
    println!("cargo:rerun-if-changed={}", web.display());
    let mut files = Vec::new();
    walk(&web, &web, &mut files);
    files.sort();
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("web_assets.rs");
    let mut f = fs::File::create(&out).expect("create web_assets.rs");
    writeln!(f, "pub static FILES: &[(&str, &[u8])] = &[").unwrap();
    for (name, path) in &files {
        writeln!(f, "    ({:?}, include_bytes!({:?})),", name, path.display().to_string()).unwrap();
    }
    writeln!(f, "];").unwrap();
}
