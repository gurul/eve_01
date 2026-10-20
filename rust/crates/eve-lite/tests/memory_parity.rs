//! test_lite_memory_parity.py: eve lite's build_memory and dream are full eve's daemon's, copied (eve lite never links
//! eve-daemon, so it keeps its own copy of the steps), and this file holds the copy to the original.
//!
//! * Statement for statement: each copied item's source, without its comments, is the daemon's (`build_memory_with`,
//!   `dream_loop`, the mem0 wrapper and its bookkeeping, the openers' defaults). A change to either side fails here
//!   until the other follows. Positive control: a copy with one step changed is caught.
//! * File for file: on two temporary homes seeded alike (a retired debrief store and mem0 folder for the move), the
//!   daemon's build and lite's build the same memory and leave the same files, under full eve and under eve lite
//!   (where no git runs, so the records keep no history). Each edition runs in a child process: HOME and the memory
//!   switches are this process's environment, as the Python test sets them in `os.environ`.
//! * The dream: lite starts it as background work that the shutdown ends; with no memory or no client, no dream.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::*;
use serde_json::{json, Value};

const LITE_SRC: &str = include_str!("../src/lib.rs");
const DAEMON_SRC: &str = include_str!("../../eve-daemon/src/run.rs");
const PLACEHOLDER_KEY: &str = "CANARY-NOT-A-KEY-7f3c";
const MEMORY_SWITCHES: &[&str] = &[
    "EVE_MEMORY", "EVE_RECORDS", "EVE_MEMORY_DIR", "EVE_NOTES_DIR", "EVE_DEBRIEF_DIR", "EVE_MEM0", "EVE_MEM0_EMBEDDER",
    "EVE_MEMORY_HYBRID", "EVE_MEMORY_RETRY", "EVE_MEMORY_TIMEARGS", "EVE_PROFILE", "OPENROUTER_API_KEY",
];

// ---- statement for statement ---------------------------------------------------------------------------------------

/// The item that starts at `head` in `src`: through the brace that closes its first `{` (strings here hold only
/// balanced braces).
fn item(src: &str, head: &str) -> String {
    let start = src.find(head).unwrap_or_else(|| panic!("no {head:?}"));
    let open = start + src[start..].find('{').unwrap();
    let mut depth = 0usize;
    for (i, c) in src[open..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return src[start..open + i + 1].to_string();
                }
            }
            _ => {}
        }
    }
    panic!("{head:?} never closes");
}

/// The statements of an item: comment lines dropped, whitespace collapsed, one entry per line of code.
fn steps(code: &str) -> Vec<String> {
    code.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with("//"))
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect()
}

/// Where the copy differs from the original; [] when it is the same.
fn differences(original: &str, copied: &str) -> Vec<String> {
    let (a, b) = (steps(original), steps(copied));
    if a == b {
        return vec![];
    }
    let mut out = vec![];
    if a.len() != b.len() {
        out.push(format!("{} lines in eve-daemon, {} in eve-lite", a.len(), b.len()));
    }
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        if x != y {
            out.push(format!("line {}: {x:?} / {y:?}", i + 1));
        }
    }
    out
}

const COPIED: &[&str] = &[
    "pub fn build_memory_with(",
    "pub fn dream_loop(",
    "impl MeaningIndex for Mem0Index",
    "fn remember_mem0(",
    "pub fn mem0_of(",
    "impl Default for MemoryOpeners",
];

#[test]
fn the_copy_is_the_daemons_statement_for_statement() {
    for head in COPIED {
        let diff = differences(&item(DAEMON_SRC, head), &item(LITE_SRC, head));
        assert_eq!(diff, Vec::<String>::new(), "{head}: eve-lite's copy no longer matches eve-daemon's");
    }
}

#[test]
fn the_statement_check_catches_a_changed_step() {
    // Positive controls: the copy with the index worker off, with ensure_repo left out, and with the dream's client
    // check dropped each differ from the daemon's.
    let (original, copied) = (item(DAEMON_SRC, COPIED[0]), item(LITE_SRC, COPIED[0]));
    assert!(copied.contains("background: true"));
    assert!(!differences(&original, &copied.replace("background: true", "background: false")).is_empty());
    let no_repo: String = copied.lines().filter(|l| !l.contains("ensure_repo")).collect::<Vec<_>>().join("\n");
    assert!(!differences(&original, &no_repo).is_empty());
    let dream = item(LITE_SRC, COPIED[1]);
    let unchecked: String = dream.lines().filter(|l| !l.contains("client?")).collect::<Vec<_>>().join("\n");
    assert!(!differences(&item(DAEMON_SRC, COPIED[1]), &unchecked).is_empty());
    // a comment is not a step
    assert!(differences(&original, &copied.replace("{\n", "{\n        // a new comment\n")).is_empty());
    assert_eq!(differences(&original, &copied), Vec::<String>::new()); // and the real copy is still the same
}

// ---- file for file --------------------------------------------------------------------------------------------------

/// A home as an older eve left it: the retired debrief store and mem0 folder, which the move takes in.
fn seed(home: &Path) {
    let legacy = home.join(".config/eve-bridge/debrief");
    std::fs::create_dir_all(legacy.join("records")).unwrap();
    std::fs::write(legacy.join("records/people.md"), "# People\n\n- Sam likes green tea.\n").unwrap();
    std::fs::create_dir_all(legacy.join("sessions")).unwrap();
    std::fs::write(legacy.join("sessions/2026-09-01.md"), "a session\n").unwrap();
    let old_mem0 = home.join(".config/eve-bridge/mem0");
    std::fs::create_dir_all(&old_mem0).unwrap();
    std::fs::write(old_mem0.join("placeholder.txt"), "an index file\n").unwrap();
}

/// Every path under `home`, relative, with each file's bytes (None for a folder). Inside a git folder only the names
/// count (git writes its own clock into some of its files), and the move's marker counts without its time.
fn tree(home: &Path) -> BTreeMap<String, Option<Vec<u8>>> {
    fn walk(home: &Path, dir: &Path, out: &mut BTreeMap<String, Option<Vec<u8>>>) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let path = entry.path();
            let rel = path.strip_prefix(home).unwrap().to_string_lossy().into_owned();
            if path.is_dir() {
                out.insert(rel, None);
                walk(home, &path, out);
            } else if format!("/{rel}").contains("/.git/") {
                out.insert(rel, Some(b"<git>".to_vec()));
            } else if path.file_name().is_some_and(|n| n == ".migrated") {
                let mut counts: serde_json::Map<String, Value> =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                counts.remove("at");
                let sorted: BTreeMap<String, Value> = counts.into_iter().collect();
                out.insert(rel, Some(serde_json::to_vec(&sorted).unwrap()));
            } else {
                out.insert(rel, Some(std::fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(home, home, &mut out);
    out
}

/// What a memory is made of, with every path relative to its home.
fn shape(memory: Option<&Arc<eve_memory::Memory>>, home: &Path) -> Value {
    let Some(memory) = memory else { return Value::Null };
    let rel = |p: &Path| p.strip_prefix(home).map(|r| r.to_string_lossy().into_owned()).unwrap_or_else(|_| "outside".into());
    let cfg = &memory.cfg;
    json!({
        "store": rel(&cfg.store), "notes": rel(&cfg.notes), "records": rel(&cfg.records_dir()),
        "transcripts_dir": rel(&cfg.transcripts_dir()), "mem0": rel(&cfg.mem0_dir()), "archive": rel(&cfg.archive_dir()),
        "transcripts": [rel(&memory.transcripts.root), memory.transcripts.archive.as_deref().map(rel)],
        "index": memory.index.is_some(),
        "switches": [memory.hybrid, memory.timeargs, memory.retry],
        "chat_log": memory.chat_log().is_some(),
    })
}

type Build = fn(&eve_records::RecallConfig) -> Option<Arc<eve_memory::Memory>>;

fn daemon_build(cfg: &eve_records::RecallConfig) -> Option<Arc<eve_memory::Memory>> {
    eve_daemon::run::build_memory_with(cfg, None, &eve_daemon::run::MemoryOpeners::default())
}

/// One side's build on its own home, under `env` alone of the memory switches.
fn build_on(home: &Path, build: Build, env: &[(&str, &str)]) -> (Value, BTreeMap<String, Option<Vec<u8>>>) {
    std::fs::create_dir_all(home).unwrap();
    seed(home);
    std::env::set_var("HOME", home);
    for name in MEMORY_SWITCHES {
        std::env::remove_var(name);
    }
    for (name, value) in env {
        std::env::set_var(name, value);
    }
    let memory = build(&eve_records::configured(None));
    (shape(memory.as_ref(), home), tree(home))
}

fn editions(edition: &str) -> Vec<(&'static str, &'static str)> {
    match edition {
        // eve lite's memory switches as the profile forces them
        "lite" => vec![
            ("EVE_PROFILE", "lite"),
            ("EVE_MEMORY", "1"),
            ("EVE_MEM0_EMBEDDER", "openrouter"),
            ("OPENROUTER_API_KEY", PLACEHOLDER_KEY),
        ],
        _ => vec![("EVE_MEMORY", "1"), ("OPENROUTER_API_KEY", PLACEHOLDER_KEY)],
    }
}

fn both_build_the_same(name: &str, edition: &str) {
    if is_child(name) {
        child_start(name);
        let base = PathBuf::from(std::env::var("TMPDIR").unwrap());
        let env = editions(edition);
        let (daemon_shape, daemon_tree) = build_on(&base.join("daemon-home"), daemon_build, &env);
        let (lite_shape, lite_tree) = build_on(&base.join("lite-home"), eve_lite::build_memory, &env);
        assert!(!daemon_shape.is_null());
        assert_eq!(lite_shape, daemon_shape);
        assert_eq!(lite_tree.keys().collect::<Vec<_>>(), daemon_tree.keys().collect::<Vec<_>>());
        assert_eq!(lite_tree, daemon_tree);
        // the move took the old store in, on both sides alike: its records, its sessions to the archive, the old index
        assert!(lite_tree.contains_key(".config/eve-bridge/memory/records/people.md"));
        assert!(lite_tree.contains_key(".config/eve-bridge/memory/archive/sessions/2026-09-01.md"));
        assert!(lite_tree.contains_key(".config/eve-bridge/memory/mem0/placeholder.txt"));
        assert!(!lite_tree.contains_key(".config/eve-bridge/debrief/records/people.md"));
        let git = ".config/eve-bridge/memory/records/.git";
        if edition == "lite" {
            assert!(!lite_tree.contains_key(git)); // eve lite runs no git: the records keep no history
        }
        eprintln!("{edition}: records are a git repository: {}", lite_tree.contains_key(git));
        return;
    }
    let b = make_box();
    run_child(name, &b, Child { path: Some(LITE_PATH.into()), ..Child::default() });
}

#[test]
fn both_build_the_same_memory_on_a_temporary_home_lite() {
    both_build_the_same("both_build_the_same_memory_on_a_temporary_home_lite", "lite");
}

#[test]
fn both_build_the_same_memory_on_a_temporary_home_full() {
    both_build_the_same("both_build_the_same_memory_on_a_temporary_home_full", "full");
}

#[test]
fn memory_off_builds_nothing_on_either_side() {
    const NAME: &str = "memory_off_builds_nothing_on_either_side";
    if is_child(NAME) {
        child_start(NAME);
        let base = PathBuf::from(std::env::var("TMPDIR").unwrap());
        for (side, build) in [("daemon", daemon_build as Build), ("lite", eve_lite::build_memory as Build)] {
            let (shape, tree) = build_on(&base.join(side), build, &[("EVE_MEMORY", "0")]);
            assert!(shape.is_null(), "{side}");
            assert!(!tree.contains_key(".config/eve-bridge/memory"), "{side}"); // with memory off nothing is touched
            assert!(tree.contains_key(".config/eve-bridge/debrief/records/people.md"), "{side}");
        }
        return;
    }
    let b = make_box();
    run_child(NAME, &b, Child::default());
}

// ---- the dream ------------------------------------------------------------------------------------------------------

/// records.make_client's stand-in: a client that is never asked (the loop only waits for the shutdown).
struct Quiet;

impl eve_records::DreamClient for Quiet {
    fn reconcile(&self, _body: &str) -> anyhow::Result<String> {
        anyhow::bail!("never asked")
    }
    fn consolidate(&self, _body: &str) -> anyhow::Result<String> {
        anyhow::bail!("never asked")
    }
}

fn a_memory(root: &Path) -> Arc<eve_memory::Memory> {
    let cfg = eve_records::RecallConfig::new(root.join("memory"), root.join("notes"));
    let transcripts = Arc::new(eve_transcripts::Transcripts::new(
        eve_transcripts::TranscriptConfig::new(true, root.join("memory/transcripts")),
        None,
        None,
    ));
    Arc::new(eve_memory::Memory::with_options(cfg, transcripts, None, eve_memory::MemoryOptions::default()))
}

/// Python patches `dream.Dreamer` to record what each side hands it and runs `Daemon._start_dream` beside
/// `lite.start_dream`. Here lite's start is run whole (named, held, ended by the shutdown, let go), and full eve's
/// `dream_loop` (the step `Daemon::start_dream` runs, which needs a whole Daemon) is given the same memory, config,
/// client and index; that both hand the Dreamer the same four things is the statement check's
/// (`the_copy_is_the_daemons_statement_for_statement`).
#[test]
fn both_start_the_same_dream() {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let tmp = tempfile::tempdir().unwrap();
        let memory = a_memory(tmp.path());
        let cfg = memory.cfg.clone();
        let (tx, rx) = tokio::sync::watch::channel(false);
        let background = eve_lite::Background::default();
        let client: eve_lite::MakeDreamClient = Arc::new(|| Some(Arc::new(Quiet) as Arc<dyn eve_records::DreamClient + Send + Sync>));
        let task = eve_lite::start_dream_with(Some(&memory), &cfg, rx, &background, &client).expect("a dream");
        assert_eq!(task.name(), "memory-dream");
        assert_eq!(background.names(), vec!["memory-dream"]); // held, so the shutdown drains it
        tx.send_replace(true);
        tokio::time::timeout(Duration::from_secs(5), task.join()).await.expect("ended with the shutdown");
        assert!(background.names().is_empty()); // and let go once it ended
        // the daemon's loop over the same memory is the same kind of thing: Some with both, as lite's copy
        let (_tx2, rx2) = tokio::sync::watch::channel(true);
        let index = eve_lite::mem0_of(&memory).map(|i| i as Arc<dyn eve_dream::DreamIndex>);
        assert!(eve_daemon::run::dream_loop(Some(&memory), &cfg, Some(Arc::new(Quiet)), index, rx2).is_some());
    });
}

#[test]
fn no_memory_or_no_client_is_no_dream_on_either_side() {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(async {
        let tmp = tempfile::tempdir().unwrap();
        let memory = a_memory(tmp.path());
        let cfg = memory.cfg.clone();
        let some: eve_lite::MakeDreamClient = Arc::new(|| Some(Arc::new(Quiet) as Arc<dyn eve_records::DreamClient + Send + Sync>));
        let none: eve_lite::MakeDreamClient = Arc::new(|| None);
        for (given, client) in [(None, &some), (Some(&memory), &none)] {
            let background = eve_lite::Background::default();
            let rx = tokio::sync::watch::channel(false).1;
            assert!(eve_lite::start_dream_with(given, &cfg, rx, &background, client).is_none());
            assert!(background.names().is_empty());
            let rx = tokio::sync::watch::channel(false).1;
            let made = client();
            let lite = eve_lite::dream_loop(given, &cfg, made.clone(), None, rx.clone()).is_some();
            let daemon = eve_daemon::run::dream_loop(given, &cfg, made, None, rx).is_some();
            assert!(!lite && !daemon);
        }
    });
}
