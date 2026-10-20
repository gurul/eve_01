//! test_lite_daemon.py's source checks, on the crate graph and this crate's source.
//!
//! * Nothing eve lite links reaches full eve's daemon or BLE: the closure of eve-lite's path dependencies (the
//!   Cargo manifests, as Python's walk of imports) leaves out eve-daemon, eve-ble and eve-bridge (the CLI, which links
//!   the daemon). Rust links every crate statically, so this one walk stands for Python's two checks (the import walk
//!   in the source, and sys.modules after a run). Positive control: the same walk from eve-daemon reaches eve-ble.
//! * eve lite reads one switch of its own, the IPC address, and the profile has the reason for it.

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

fn crates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().to_path_buf()
}

/// The path dependencies of one crate's `[dependencies]` (not its dev or build dependencies), by crate folder name.
fn path_deps(name: &str) -> Vec<String> {
    let manifest = std::fs::read_to_string(crates_dir().join(name).join("Cargo.toml")).unwrap_or_default();
    let mut out = vec![];
    let mut in_deps = false;
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            // [dependencies] and [target.'cfg(...)'.dependencies] count; dev and build dependencies do not
            in_deps = line == "[dependencies]" || (line.starts_with("[target.") && line.ends_with(".dependencies]"));
            continue;
        }
        if !in_deps || line.starts_with('#') {
            continue;
        }
        if let Some(at) = line.find("path = \"../") {
            let rest = &line[at + "path = \"../".len()..];
            out.push(rest.split('"').next().unwrap().trim_end_matches('/').to_string());
        }
    }
    out
}

fn closure(roots: &[&str]) -> BTreeSet<String> {
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<String> = roots.iter().map(|r| r.to_string()).collect();
    while let Some(name) = queue.pop_front() {
        if seen.insert(name.clone()) {
            queue.extend(path_deps(&name));
        }
    }
    seen
}

#[test]
fn no_dependency_of_lite_reaches_full_eves_daemon() {
    let reached = closure(&["eve-lite"]);
    for needed in ["eve-lite", "eve-brain", "eve-lite-web", "eve-lite-rtc", "eve-loop-watchdog"] {
        assert!(reached.contains(needed), "{needed} not reached: {reached:?}");
    }
    for banned in ["eve-daemon", "eve-ble", "eve-bridge"] {
        assert!(!reached.contains(banned), "eve-lite reaches {banned}");
    }
    // positive control: the same walk from full eve's daemon reaches BLE
    let daemon = closure(&["eve-daemon"]);
    assert!(daemon.contains("eve-ble"), "{daemon:?}");
}

/// The environment switches a source reads: string literals that are a switch's name alone (`"EVE_..."` or an API
/// key's), on lines of code (not comments).
fn switches_in(src: &str) -> BTreeSet<String> {
    let name = regex::Regex::new(r#""((?:EVE|OPENROUTER|OPENAI|ANTHROPIC|COMPOSIO)_[A-Z0-9_]+)""#).unwrap();
    src.lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .flat_map(|l| name.captures_iter(l).map(|c| c[1].to_string()).collect::<Vec<_>>())
        .collect()
}

#[test]
fn lite_reads_only_classified_switches() {
    let mut found = BTreeSet::new();
    for file in ["lib.rs", "apps.rs", "logging.rs"] {
        let src = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src").join(file)).unwrap();
        found.extend(switches_in(&src));
    }
    assert_eq!(found, BTreeSet::from(["EVE_BRIDGE_SOCK".to_string()]));
    assert!(found.iter().all(|s| eve_profile::LITE_CLASSIFIED.contains_key(s)));
    // positive control: a read of a switch is found, a comment or a log line naming one is not
    let sample = "let x = std::env::var(\"EVE_SOMETHING\");\n// \"EVE_COMMENT\"\ninfo!(\"memory: off (EVE_MEMORY)\");";
    assert_eq!(switches_in(sample), BTreeSet::from(["EVE_SOMETHING".to_string()]));
}
