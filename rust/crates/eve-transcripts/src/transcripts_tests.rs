//! transcripts.py's tests (bridge/tests/test_transcripts.py): the local per-day transcript store — guards, day
//! boundary, blocks, search, forget.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use chrono::{Duration, FixedOffset, Local, Offset, TimeZone};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use crate::py;
use crate::transcripts::*;

const SENTINEL: &str = "zebra-sentinel-7Q";

fn tz() -> FixedOffset {
    Local
        .with_ymd_and_hms(2026, 9, 21, 12, 0, 0)
        .unwrap()
        .offset()
        .fix()
}

fn at(day: u32, hh: u32, mm: u32, ss: u32) -> Dt {
    tz().with_ymd_and_hms(2026, 9, day, hh, mm, ss).unwrap()
}

#[derive(Clone)]
struct Clock(Arc<Mutex<Dt>>);

impl Clock {
    fn wall(&self) -> Wall {
        let c = self.0.clone();
        Arc::new(move || *c.lock())
    }
}

fn store_with(tmp: &Path, now: Dt, archive: Option<PathBuf>) -> (Transcripts, Clock) {
    let clock = Clock(Arc::new(Mutex::new(now)));
    let t = Transcripts::new(
        TranscriptConfig::new(true, tmp.join("tx")),
        archive,
        Some(clock.wall()),
    );
    (t, clock)
}

fn store(tmp: &Path, now: Dt) -> (Transcripts, Clock) {
    store_with(tmp, now, None)
}

/// `t.append(ch, conv, who, kind, text, now=when)`.
fn put(t: &Transcripts, ch: &str, conv: &str, who: &str, kind: &str, text: &str, when: Dt) -> bool {
    t.append_with(
        ch,
        conv,
        who,
        kind,
        text,
        Append {
            now: Some(when),
            ..Append::default()
        },
    )
}

fn put_tool(t: &Transcripts, ch: &str, conv: &str, text: &str, tool: &str, when: Dt) -> bool {
    t.append_with(
        ch,
        conv,
        "eve",
        "tool",
        text,
        Append {
            tool: tool.into(),
            now: Some(when),
            ..Append::default()
        },
    )
}

fn raw_lines(t: &Transcripts, day: &str) -> Vec<Map<String, Value>> {
    fs::read_to_string(t.path_of(day))
        .unwrap()
        .lines()
        .filter(|r| !r.trim().is_empty())
        .map(|r| {
            serde_json::from_str::<Value>(r)
                .unwrap()
                .as_object()
                .unwrap()
                .clone()
        })
        .collect()
}

fn texts(lines: &[Arc<Line>]) -> Vec<String> {
    lines.iter().map(|l| field(l, "text").to_string()).collect()
}

fn hits(res: &Value) -> Vec<Value> {
    res["hits"].as_array().cloned().unwrap_or_default()
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// Run `f` with every log line this thread writes at `level` and above captured; → the captured text.
///
/// One global subscriber at TRACE writes into a per-thread buffer while a capture is on, so every callsite stays
/// interesting whatever other tests run at the same time (a scoped subscriber races tracing's interest cache).
fn capture_logs(level: tracing::Level, f: impl FnOnce()) -> String {
    use std::cell::RefCell;
    thread_local! {
        static SINK: RefCell<Option<(tracing::Level, Vec<u8>)>> = const { RefCell::new(None) };
    }
    struct ThreadSink;
    impl std::io::Write for ThreadSink {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            SINK.with(|s| {
                if let Some((level, buf)) = s.borrow_mut().as_mut() {
                    let text = String::from_utf8_lossy(b);
                    let shown = ["ERROR", "WARN", "INFO", "DEBUG", "TRACE"]
                        .iter()
                        .take_while(|l| **l != level.as_str())
                        .chain(std::iter::once(&level.as_str()))
                        .any(|l| text.contains(&format!(" {l} ")));
                    if shown {
                        buf.extend_from_slice(b);
                    }
                }
            });
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let sub = tracing_subscriber::fmt()
            .with_writer(|| ThreadSink)
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let _ = tracing::subscriber::set_global_default(sub);
    });
    SINK.with(|s| *s.borrow_mut() = Some((level, Vec::new())));
    f();
    let got = SINK
        .with(|s| s.borrow_mut().take())
        .map(|(_, b)| b)
        .unwrap_or_default();
    String::from_utf8_lossy(&got).into_owned()
}

fn warnings(log: &str) -> usize {
    log.lines().filter(|l| l.contains(" WARN ")).count()
}

// ---- config ---------------------------------------------------------------------------------------------------------

#[test]
fn test_configured_is_off_by_default_and_on_with_memory_or_the_legacy_records_switch() {
    let env = |pairs: &[(&str, &str)]| -> std::collections::HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    };
    let off = configured(Some(&env(&[])));
    assert!(!off.enabled && !MEMORY_DEFAULT);
    assert_eq!(
        off.root,
        py::expand_user(Path::new("~/.config/eve-bridge/memory/transcripts"))
    );
    assert!(configured(Some(&env(&[("EVE_MEMORY", "1")]))).enabled);
    assert!(configured(Some(&env(&[("EVE_RECORDS", "1")]))).enabled); // the older switch still turns it on
    assert!(!configured(Some(&env(&[("EVE_MEMORY", "0"), ("EVE_RECORDS", "0")]))).enabled);
    assert_eq!(
        configured(Some(&env(&[("EVE_MEMORY_DIR", "/x/mem")]))).root,
        PathBuf::from("/x/mem/transcripts")
    );
    assert_eq!(
        (DAY_START_HOUR, VOICE_CHARS, BACKEND_CHARS),
        (4, 4000, 16000)
    );
}

#[test]
fn test_a_disabled_store_writes_and_reads_nothing() {
    let d = tmp();
    let t = Transcripts::new(
        TranscriptConfig::new(false, d.path().join("tx")),
        None,
        None,
    );
    assert!(!t.append("voice", "v-1", "owner", "say", "hi"));
    assert!(!d.path().join("tx").exists() && t.days().is_empty() && t.today_block(1000).is_empty());
}

// ---- day boundary and files -----------------------------------------------------------------------------------------

#[test]
fn test_the_day_starts_at_four() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    assert_eq!(t.day_of(at(22, 3, 59, 0)), "2026-09-21");
    assert_eq!(t.day_of(at(22, 4, 0, 0)), "2026-09-22");
    put(&t, "voice", "v-1", "owner", "say", "late", at(22, 3, 59, 0));
    put(&t, "voice", "v-1", "owner", "say", "early", at(22, 4, 0, 0));
    assert_eq!(texts(&t.lines("2026-09-21")), ["late"]);
    assert_eq!(texts(&t.lines("2026-09-22")), ["early"]);
    assert_eq!(t.days(), ["2026-09-21", "2026-09-22"]);
}

#[test]
fn test_modes_are_private_whatever_the_umask() {
    let d = tmp();
    // SAFETY: umask only swaps the process's file-creation mask; 022 is the usual one, so other tests are unaffected.
    let old = unsafe { libc::umask(0o022) };
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    t.append("telegram", "t-1", "owner", "say", "hello");
    // SAFETY: as above, restoring the mask.
    unsafe { libc::umask(old) };
    assert_eq!(
        fs::metadata(&t.root).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(t.path_of("2026-09-21"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert!(t.root.join(NEVER_INDEX).exists());
}

#[test]
fn test_a_line_has_the_envelope_and_text_is_capped() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    let conv = t.new_conv("voice");
    assert!(put_tool(
        &t,
        "voice",
        &conv,
        &"x".repeat(30000),
        "web_search",
        at(21, 9, 5, 0)
    ));
    let lines = raw_lines(&t, "2026-09-21");
    assert_eq!(lines.len(), 1);
    let line = &lines[0];
    let keys: std::collections::BTreeSet<&str> = line.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["ch", "conv", "kind", "text", "tool", "ts", "who"]
            .into_iter()
            .collect()
    );
    let ts = line["ts"].as_str().unwrap();
    assert!(ts.starts_with("2026-09-21T09:05:00") && "+-".contains(&ts[19..20]));
    assert_eq!(line["text"].as_str().unwrap().len(), MAX_TEXT_CHARS);
    assert_eq!(line["tool"], "web_search");
    // nonsense is refused, not written
    assert!(!t.append("fax", &conv, "owner", "say", "x"));
    assert!(!t.append("voice", &conv, "stranger", "say", "x"));
    assert_eq!(t.stats().rejected, 2);
    assert_eq!(raw_lines(&t, "2026-09-21").len(), 1);
}

#[test]
fn test_conversation_ids_are_unique_and_carry_the_channel() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    let mut ids = std::collections::HashSet::new();
    for _ in 0..2000 {
        ids.insert(t.new_conv("voice"));
    }
    for _ in 0..2000 {
        ids.insert(t.new_conv("telegram"));
    }
    assert_eq!(ids.len(), 4000);
    assert!(ids.iter().all(|i| CONV.is_match(i)));
    let prefixes: std::collections::BTreeSet<char> =
        ids.iter().map(|i| i.chars().next().unwrap()).collect();
    assert_eq!(prefixes, ['t', 'v'].into_iter().collect());
}

// ---- guards ---------------------------------------------------------------------------------------------------------

#[test]
fn test_a_root_inside_a_git_work_tree_is_refused_with_one_warning() {
    let d = tmp();
    let repo = d.path().join("repo");
    fs::create_dir(&repo).unwrap();
    let ok = std::process::Command::new("git")
        .args(["init", "-q"])
        .arg(&repo)
        .status()
        .unwrap();
    assert!(ok.success());
    let mut t = None;
    let log = capture_logs(tracing::Level::WARN, || {
        t = Some(Transcripts::new(
            TranscriptConfig::new(true, repo.join("deep").join("tx")),
            None,
            None,
        ));
    });
    let t = t.unwrap();
    assert!(!t.enabled);
    assert!(!t.append("voice", "v-1", "owner", "say", "hi"));
    assert!(!repo.join("deep").exists());
    assert_eq!(warnings(&log), 1);
    assert!(log.contains("git work tree"));
    // positive control: the same shape of root with no .git writes
    let plain = d.path().join("plain");
    fs::create_dir(&plain).unwrap();
    let ok = Transcripts::new(
        TranscriptConfig::new(true, plain.join("deep").join("tx")),
        None,
        None,
    );
    assert!(ok.enabled && ok.append("voice", "v-1", "owner", "say", "hi"));
}

#[test]
fn test_synced_folders_and_symlinks_are_refused() {
    let d = tmp();
    let home = d.path().join("home");
    fs::create_dir_all(home.join("Documents")).unwrap();
    let cfg = |root: PathBuf| TranscriptConfig {
        enabled: true,
        root,
        home: Some(home.clone()),
    };
    for rel in [
        "Documents/eve",
        "Desktop/tx",
        "Library/Mobile Documents/x",
        "Library/CloudStorage/y/tx",
    ] {
        assert!(
            refusal_in(&home.join(rel), &home).contains("syncs"),
            "{rel}"
        );
        let t = Transcripts::new(cfg(home.join(rel)), None, None);
        assert!(!t.enabled && !t.append("voice", "v-1", "owner", "say", "x"));
    }
    let target = d.path().join("target");
    fs::create_dir(&target).unwrap();
    let link = d.path().join("link");
    std::os::unix::fs::symlink(&target, &link).unwrap();
    assert!(refusal_in(&link, &home).contains("symlink"));
    assert!(!Transcripts::new(cfg(link), None, None).enabled);
    // control: a private folder under the same fake home is fine
    assert_eq!(refusal_in(&home.join(".config").join("tx"), &home), "");
    assert!(Transcripts::new(cfg(home.join(".config").join("tx")), None, None).enabled);
}

// ---- failure is silence ---------------------------------------------------------------------------------------------

#[test]
fn test_an_unwritable_folder_counts_failures_and_raises_nothing() {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return; // root ignores directory permissions
    }
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    fs::set_permissions(&t.root, fs::Permissions::from_mode(0o500)).unwrap();
    let log = capture_logs(tracing::Level::WARN, || {
        for _ in 0..3 {
            assert!(!t.append("voice", "v-1", "owner", "say", SENTINEL));
        }
    });
    fs::set_permissions(&t.root, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(t.stats().failed, 3);
    assert_eq!(warnings(&log), 1); // once an hour, not per line
    assert!(!log.contains(SENTINEL));
}

#[test]
fn test_a_torn_last_line_is_skipped_and_the_next_line_survives() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    put(&t, "voice", "v-1", "owner", "say", "whole", at(21, 9, 0, 0));
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(t.path_of("2026-09-21"))
        .unwrap();
    f.write_all(br#"{"ts": "2026-09-21T09:01:00+00:00", "ch": "voice", "te"#)
        .unwrap(); // a crash mid-write
    drop(f);
    assert_eq!(texts(&t.lines("2026-09-21")), ["whole"]);
    put(&t, "voice", "v-1", "eve", "say", "after", at(21, 9, 2, 0));
    assert_eq!(texts(&t.lines("2026-09-21")), ["whole", "after"]);
}

#[test]
fn test_eight_threads_of_appends_give_whole_lines() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    std::thread::scope(|s| {
        for n in 0..8 {
            let t = &t;
            s.spawn(move || {
                for i in 0..500 {
                    let ch = if n % 2 == 1 { "telegram" } else { "voice" };
                    t.append(
                        ch,
                        &format!("v-{n}"),
                        "owner",
                        "say",
                        &format!("thread {n} line {i} {}", "y".repeat(200)),
                    );
                }
            });
        }
    });
    assert_eq!(raw_lines(&t, "2026-09-21").len(), 4000); // every one parses: none torn or interleaved
    assert_eq!(t.lines("2026-09-21").len(), 4000);
    assert_eq!(t.stats().appended, 4000);
}

#[test]
fn test_nothing_said_reaches_the_log() {
    let d = tmp();
    let log = capture_logs(tracing::Level::DEBUG, || {
        let (t, _) = store(d.path(), at(21, 12, 0, 0));
        let conv = t.new_conv("voice");
        t.append(
            "voice",
            &conv,
            "owner",
            "say",
            &format!("my {SENTINEL} is here"),
        );
        t.close("voice", &conv, None);
        t.today_block(4000);
        t.search(SENTINEL);
        t.find(SENTINEL);
        assert_eq!(t.redact(SENTINEL, false), 1);
    });
    assert!(log.contains("forgot 1 lines")); // counts do get logged
    assert!(!log.contains(SENTINEL));
}

#[test]
fn test_no_network_is_touched() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    put(
        &t,
        "voice",
        "v-1",
        "owner",
        "say",
        "the red kite",
        at(21, 9, 0, 0),
    );
    assert!(t
        .read("2026-09-21", "", "", READ_MAX_CHARS)
        .contains("red kite"));
    assert_eq!(hits(&t.search("kite"))[0]["text"], "the red kite");
    let src = include_str!("transcripts.rs");
    for lib in [
        "std::net",
        "reqwest",
        "hyper",
        "ureq",
        "eve_openrouter",
        "TcpStream",
        "UdpSocket",
    ] {
        assert!(!src.contains(lib), "{lib}");
    }
}

// ---- today block ----------------------------------------------------------------------------------------------------

#[test]
fn test_today_block_renders_both_channels_and_drops_commands_relays_and_closes() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 18, 0, 0));
    assert_eq!(t.today_block(4000), "");
    put(
        &t,
        "voice",
        "v-1",
        "owner",
        "say",
        "I moved the  meeting\nto Friday",
        at(21, 9, 5, 0),
    );
    put_tool(
        &t,
        "voice",
        "v-1",
        "found it",
        "web_search",
        at(21, 9, 6, 0),
    );
    put(&t, "voice", "v-1", "eve", "say", "Noted.", at(21, 9, 6, 0));
    t.close("voice", "v-1", Some(at(21, 9, 7, 0)));
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "command",
        "claude on",
        at(21, 10, 0, 0),
    );
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "relay",
        "fix the build",
        at(21, 10, 1, 0),
    );
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "image",
        "a receipt",
        at(21, 10, 2, 0),
    );
    put(
        &t,
        "voice",
        "v-0",
        "owner",
        "say",
        "yesterday's line",
        at(20, 23, 0, 0),
    );
    assert_eq!(
        t.today_block(4000).lines().collect::<Vec<_>>(),
        [
            "09:05 (spoken) Owner: I moved the meeting to Friday",
            "09:06 (spoken) eve (web search): found it",
            "09:06 (spoken) eve: Noted.",
            "10:02 (texted) Owner (photo): a receipt",
        ]
    );
}

#[test]
fn test_today_block_keeps_the_newest_lines_within_budget() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 23, 0, 0));
    for i in 0..300 {
        let who = if i % 2 == 1 { "owner" } else { "eve" };
        put(
            &t,
            "telegram",
            "t-1",
            who,
            "say",
            &format!("line {i:03} {}", "w".repeat(60)),
            at(21, 5, 0, 0) + Duration::minutes(i),
        );
    }
    let block = t.today_block(4000);
    assert!(!block.is_empty() && py::len(&block) <= 4000);
    let rows: Vec<&str> = block.lines().collect();
    assert!(
        rows[0].starts_with("(about ")
            && rows[0].ends_with("earlier lines today: memory_search finds them)")
    );
    assert!(rows.last().unwrap().contains("line 299")); // the newest line is kept
    let kept: Vec<usize> = rows[1..]
        .iter()
        .map(|r| r.split("line ").nth(1).unwrap()[..3].parse().unwrap())
        .collect();
    assert_eq!(kept, (kept[0]..300).collect::<Vec<_>>()); // a contiguous newest run
    let n: usize = rows[0].split_whitespace().nth(1).unwrap().parse().unwrap();
    assert!(n.is_multiple_of(HEADER_BUCKET) && n >= kept[0]); // bucketed count
    for budget in [500, 1000, 2500, 12000] {
        assert!(py::len(&t.today_block(budget)) as i64 <= budget);
    }
}

#[test]
fn test_today_block_is_a_stable_prefix_while_under_budget_and_between_cuts() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 23, 0, 0));
    for i in 0..5 {
        put(
            &t,
            "voice",
            "v-1",
            "owner",
            "say",
            &format!("first {i}"),
            at(21, 9, i, 0),
        );
    }
    let before = t.today_block(4000);
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "say",
        "then a text",
        at(21, 10, 0, 0),
    );
    let after = t.today_block(4000);
    assert!(after.starts_with(&before) && after.len() > before.len());
    // over budget: once cut, later appends extend the block rather than shifting it
    for i in 0..80 {
        put(
            &t,
            "telegram",
            "t-1",
            "eve",
            "say",
            &format!("reply {i:02} {}", "z".repeat(40)),
            at(21, 11, 0, 0) + Duration::minutes(i),
        );
    }
    let cut = t.today_block(3000);
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "say",
        "one more",
        at(21, 13, 0, 0),
    );
    assert!(t.today_block(3000).starts_with(&cut));
}

#[test]
fn test_exclude_conv_and_exclude_tail_remove_exactly_those_lines() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 23, 0, 0));
    put(
        &t,
        "voice",
        "v-1",
        "owner",
        "say",
        "voice one",
        at(21, 9, 0, 0),
    );
    for i in 0..6 {
        let who = if i % 2 == 0 { "owner" } else { "eve" };
        put(
            &t,
            "telegram",
            "t-1",
            who,
            "say",
            &format!("text {i}"),
            at(21, 10, i, 0),
        );
    }
    put_tool(
        &t,
        "telegram",
        "t-1",
        "a tool result",
        "memory_search",
        at(21, 10, 3, 0),
    );
    put(
        &t,
        "voice",
        "v-2",
        "owner",
        "say",
        "voice two",
        at(21, 11, 0, 0),
    );
    let opts = |conv: &str, tail: i64| Today {
        exclude_conv: Some(conv.into()),
        exclude_tail: tail,
        ..Today::default()
    };
    let whole = t.today_block_with(4000, &opts("t-1", 0));
    assert!(!whole.contains("text") && !whole.contains("a tool result"));
    assert!(whole.contains("voice one") && whole.contains("voice two"));
    let tail = t.today_block_with(4000, &opts("t-1", 4)); // the last 4 turns are in the history
    assert_eq!(
        tail.lines()
            .map(|r| r.split_once(": ").unwrap().1)
            .collect::<Vec<_>>(),
        [
            "voice one",
            "text 0",
            "text 1",
            "a tool result",
            "voice two"
        ]
    );
    assert_eq!(
        t.today_block_with(4000, &opts("t-9", 0)),
        t.today_block(4000)
    ); // an unknown conv drops nothing
       // `exclude` does the same for several conversations at once
    let several = Today {
        exclude: vec![("t-1".into(), 4)],
        ..Today::default()
    };
    assert_eq!(t.today_block_with(4000, &several), tail);
}

#[test]
fn test_voice_style_clips_lines_shorter() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 23, 0, 0));
    put(
        &t,
        "voice",
        "v-1",
        "owner",
        "say",
        &"a".repeat(2000),
        at(21, 9, 0, 0),
    );
    let text_row = t.today_block(4000);
    let voice_row = t.today_block_with(
        4000,
        &Today {
            style: "voice".into(),
            ..Today::default()
        },
    );
    assert_eq!(text_row.lines().count(), 1);
    assert_eq!(voice_row.lines().count(), 1);
    assert!(voice_row.len() < text_row.len() && voice_row.ends_with('…'));
}

// ---- other readers --------------------------------------------------------------------------------------------------

#[test]
fn test_conversations_are_read_whole_across_the_day_boundary() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    let late = format!("v-{}-abcd", at(22, 3, 58, 0).timestamp() * 1000);
    put(
        &t,
        "voice",
        &late,
        "owner",
        "say",
        "still up",
        at(22, 3, 58, 0),
    );
    put(&t, "voice", &late, "eve", "say", "me too", at(22, 4, 2, 0));
    t.close("voice", &late, Some(at(22, 4, 12, 0)));
    let tg = format!("t-{}-0001", at(21, 20, 0, 0).timestamp() * 1000);
    put(&t, "telegram", &tg, "owner", "say", "hi", at(21, 20, 0, 0));
    let convs = t.conversations("2026-09-21");
    assert_eq!(
        convs.iter().map(|c| c.conv.clone()).collect::<Vec<_>>(),
        [tg.clone(), late.clone()]
    );
    assert_eq!(
        convs[1],
        ConvInfo {
            conv: late.clone(),
            ch: "voice".into(),
            started: at(22, 3, 58, 0),
            ended: at(22, 4, 2, 0),
            closed: true
        }
    );
    assert!(!convs[0].closed);
    assert!(t.conversations("2026-09-22").is_empty()); // filed under the day it began, once
    assert_eq!(texts(&t.conv_lines(&late)), ["still up", "me too", ""]);
}

#[test]
fn test_last_turn_at_spans_both_channels_and_ignores_close_markers() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    assert_eq!(t.last_turn_at(), None);
    put(&t, "voice", "v-1", "owner", "say", "a", at(21, 9, 0, 0));
    put(&t, "telegram", "t-1", "owner", "say", "b", at(21, 11, 0, 0));
    t.close("voice", "v-1", Some(at(21, 12, 0, 0)));
    assert_eq!(t.last_turn_at(), Some(at(21, 11, 0, 0)));
    let (fresh, _) = store(d.path(), at(21, 12, 0, 0)); // a restart reads it back from disk
    assert_eq!(fresh.last_turn_at(), Some(at(21, 11, 0, 0)));
}

#[test]
fn test_lines_since_returns_other_channels_only() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "say",
        "too early",
        at(21, 9, 0, 0),
    );
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "say",
        "sent mid-session",
        at(21, 10, 5, 0),
    );
    put(
        &t,
        "voice",
        "v-1",
        "owner",
        "say",
        "the voice itself",
        at(21, 10, 6, 0),
    );
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "command",
        "claude off",
        at(21, 10, 7, 0),
    );
    let got = t.lines_since(at(21, 10, 0, 0), "voice");
    assert_eq!(texts(&got), ["sent mid-session"]);
}

#[test]
fn test_the_cache_is_invalidated_when_the_file_grows() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    put(&t, "voice", "v-1", "owner", "say", "one", at(21, 9, 0, 0));
    assert_eq!(t.lines("2026-09-21").len(), 1);
    assert!(Arc::ptr_eq(
        &t.lines("2026-09-21")[0],
        &t.lines("2026-09-21")[0]
    )); // cached
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(t.path_of("2026-09-21"))
        .unwrap(); // another process appends
    let line = json!({"ts": at(21, 9, 1, 0).to_rfc3339(), "ch": "telegram", "conv": "t-1", "who": "owner",
                      "kind": "say", "text": "two"});
    writeln!(f, "{}", py::dumps(&line)).unwrap();
    drop(f);
    assert_eq!(texts(&t.lines("2026-09-21")), ["one", "two"]);
}

#[test]
fn test_read_returns_a_time_window_including_after_midnight() {
    let d = tmp();
    let (t, _) = store(d.path(), at(22, 3, 0, 0));
    put(
        &t,
        "voice",
        "v-1",
        "owner",
        "say",
        "morning",
        at(21, 8, 0, 0),
    );
    put(
        &t,
        "voice",
        "v-1",
        "owner",
        "say",
        "evening",
        at(21, 22, 0, 0),
    );
    put(
        &t,
        "voice",
        "v-1",
        "owner",
        "say",
        "past midnight",
        at(22, 1, 0, 0),
    );
    let window = t.read("2026-09-21", "21:00", "", READ_MAX_CHARS);
    assert_eq!(
        window
            .lines()
            .map(|r| r.split(": ").nth(1).unwrap())
            .collect::<Vec<_>>(),
        ["evening", "past midnight"]
    );
    assert!(t
        .read("2026-09-21", "", "", READ_MAX_CHARS)
        .contains("morning"));
    assert!(!t
        .read("2026-09-21", "", "09:00", READ_MAX_CHARS)
        .contains("evening"));
    let long = t.read("2026-09-21", "", "", 80);
    assert!(py::len(&long) <= 80 + 60 && long.contains("read again from"));
}

#[test]
fn test_day_text_renders_the_whole_day_and_keeps_the_newest_over_budget() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    assert_eq!(t.day_text("2026-09-21", DAY_TEXT_CHARS as i64, false), "");
    put(&t, "voice", "v-1", "owner", "say", "hello", at(21, 9, 0, 0));
    put_tool(&t, "voice", "v-1", "sunny", "web_search", at(21, 9, 1, 0));
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "command",
        "claude on",
        at(21, 9, 2, 0),
    );
    put(&t, "telegram", "t-1", "eve", "say", "done", at(21, 9, 3, 0));
    assert_eq!(
        t.day_text("2026-09-21", DAY_TEXT_CHARS as i64, false)
            .lines()
            .collect::<Vec<_>>(),
        [
            "2026-09-21 09:00 spoken Owner: hello".to_string(),
            format!("2026-09-21 09:01 spoken eve (web search): {OUTSIDE_LEFT_OUT}"),
            "2026-09-21 09:03 texted eve: done".to_string(),
        ]
    );
    assert!(t
        .day_text("2026-09-21", DAY_TEXT_CHARS as i64, true)
        .contains("spoken eve (web search): sunny"));
    for i in 0..100 {
        put(
            &t,
            "telegram",
            "t-1",
            "owner",
            "say",
            &format!("n{i:03} {}", "q".repeat(50)),
            at(21, 10, 0, 0) + Duration::minutes(i),
        );
    }
    let text = t.day_text("2026-09-21", 1000, false);
    let rows: Vec<&str> = text.lines().collect();
    assert!(py::len(&text) <= 1000 && rows.last().unwrap().contains("n099"));
    let omitted: usize = rows[0]
        .trim_start_matches('(')
        .split_whitespace()
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(rows[0], format!("({omitted} earlier lines omitted)"));
    assert_eq!(omitted + rows.len() - 1, 103);
}

/// The dream's days/2026-09-23.md filed an event of 2026-09-24 00:08 under 2026-09-23: the transcript day runs 04:00
/// to 04:00, and its lines carried only HH:MM under "Everything said on 2026-09-23". A line past midnight stays in its
/// transcript day, and says the date it was actually said.
#[test]
fn test_day_text_dates_each_line_with_its_own_calendar_date() {
    let d = tmp();
    let (t, _) = store(d.path(), at(22, 12, 0, 0));
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "say",
        "evening",
        at(21, 23, 50, 0),
    );
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "say",
        "past midnight",
        at(22, 0, 8, 0),
    );
    assert_eq!(t.day_of(at(22, 0, 8, 0)), "2026-09-21");
    assert_eq!(
        t.day_text("2026-09-21", DAY_TEXT_CHARS as i64, false)
            .lines()
            .collect::<Vec<_>>(),
        [
            "2026-09-21 23:50 texted Owner: evening",
            "2026-09-22 00:08 texted Owner: past midnight"
        ]
    );
}

// ---- search ---------------------------------------------------------------------------------------------------------

#[test]
fn test_search_finds_dated_hits_newest_first_with_neighbours_from_the_same_conversation() {
    let d = tmp();
    let (t, _) = store(d.path(), at(22, 12, 0, 0));
    put(
        &t,
        "voice",
        "v-1",
        "owner",
        "say",
        "my sister lives by the harbour",
        at(20, 9, 0, 0),
    );
    put(
        &t,
        "voice",
        "v-1",
        "eve",
        "say",
        "Good to know.",
        at(20, 9, 1, 0),
    );
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "say",
        "unrelated interleaved line",
        at(21, 10, 0, 0),
    );
    put(
        &t,
        "voice",
        "v-2",
        "owner",
        "say",
        "we visited my sister today",
        at(21, 10, 1, 0),
    );
    put(
        &t,
        "telegram",
        "t-1",
        "eve",
        "say",
        "another interleaved line",
        at(21, 10, 2, 0),
    );
    put(
        &t,
        "voice",
        "v-2",
        "eve",
        "say",
        "How was it?",
        at(21, 10, 3, 0),
    );
    let res = t.search("Sister");
    assert_eq!(res["ok"], true);
    assert_eq!(res["truncated"], false);
    let h = hits(&res);
    assert_eq!(
        h.iter()
            .map(|x| (x["day"].as_str().unwrap(), x["time"].as_str().unwrap()))
            .collect::<Vec<_>>(),
        [("2026-09-21", "10:01"), ("2026-09-20", "09:00")]
    );
    assert!(h[0]["ch"] == "voice" && h[0]["who"] == "owner");
    assert_eq!(h[0]["before"], "");
    assert_eq!(h[0]["after"], "eve: How was it?"); // not the interleaved text
    assert_eq!(h[1]["after"], "eve: Good to know.");
    let on = |channel: &str| Search {
        channel: channel.into(),
        ..Search::default()
    };
    assert!(hits(&t.search_with("sister", &on("telegram"))).is_empty());
    assert_eq!(
        hits(&t.search_with("interleaved", &on("telegram"))).len(),
        2
    );
    assert_eq!(hits(&t.search("sister harbour"))[0]["day"], "2026-09-20"); // every term must match
    let one_day = Search {
        days_back: 1,
        ..Search::default()
    };
    assert_eq!(
        hits(&t.search_with("sister", &one_day))[0]["day"],
        "2026-09-21"
    );
    assert_eq!(hits(&t.search_with("sister", &one_day)).len(), 1);
}

#[test]
fn test_a_quoted_phrase_matches_as_written() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    put(
        &t,
        "voice",
        "v-1",
        "owner",
        "say",
        "the blue car is parked",
        at(21, 9, 0, 0),
    );
    put(
        &t,
        "voice",
        "v-1",
        "owner",
        "say",
        "the car is blue",
        at(21, 9, 1, 0),
    );
    assert_eq!(hits(&t.search("blue car")).len(), 2);
    assert_eq!(
        hits(&t.search("\"blue car\""))
            .iter()
            .map(|h| h["text"].clone())
            .collect::<Vec<_>>(),
        ["the blue car is parked"]
    );
    assert_eq!(t.search("a")["ok"], false);
    assert_eq!(t.search("\" \"")["ok"], false); // no usable term
}

#[test]
fn test_search_reads_meeting_notes_and_the_archive() {
    let d = tmp();
    let archive = d.path().join("debrief");
    fs::create_dir_all(archive.join("sessions").join("2026-09-19")).unwrap();
    fs::write(
        archive
            .join("sessions")
            .join("2026-09-19")
            .join("0930-abc.md"),
        "---\nstatus: machine-draft\n---\n\n# Plans\n\n- Owner wants a trip to the lighthouse.\n",
    )
    .unwrap();
    fs::write(
        archive.join("2026-09-18-planning.md"),
        "# The day\n\n- Lighthouse trip first raised.\n",
    )
    .unwrap();
    let (t, _) = store_with(d.path(), at(21, 12, 0, 0), Some(archive.clone()));
    let meet = t.root.join(MEETINGS_SUBDIR).join("2026-09-20");
    fs::create_dir_all(&meet).unwrap();
    fs::write(
        meet.join("1400-standup.md"),
        "---\nstatus: notes\n---\n\n# Standup\n\n## Decisions\n\n- Ship the lighthouse page Friday.\n- Budget stays flat.\n",
    )
    .unwrap();
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "say",
        "book the lighthouse tour",
        at(21, 9, 0, 0),
    );
    let h = hits(&t.search("lighthouse"));
    assert_eq!(
        h.iter()
            .map(|x| (x["day"].as_str().unwrap(), x["ch"].as_str().unwrap()))
            .collect::<Vec<_>>(),
        [
            ("2026-09-21", "telegram"),
            ("2026-09-20", "meeting"),
            ("2026-09-19", "note"),
            ("2026-09-18", "note")
        ]
    );
    let meeting = &h[1];
    assert!(meeting["time"] == "14:00" && meeting["text"] == "Ship the lighthouse page Friday.");
    assert!(meeting["before"] == "Decisions" && meeting["after"] == "Budget stays flat.");
    assert!(h[2]["time"] == "09:30" && h[3]["time"] == "");
    let only = Search {
        channel: "meeting".into(),
        ..Search::default()
    };
    assert_eq!(
        hits(&t.search_with("lighthouse", &only))
            .iter()
            .map(|x| x["ch"].clone())
            .collect::<Vec<_>>(),
        ["meeting"]
    );
    assert!(!py::dumps(&t.search("notes")["hits"]).contains("status")); // frontmatter is not searched
    let (no_archive, _) = store(d.path(), at(21, 12, 0, 0));
    let chs: std::collections::BTreeSet<String> = hits(&no_archive.search("lighthouse"))
        .iter()
        .map(|x| x["ch"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        chs,
        ["meeting".to_string(), "telegram".to_string()]
            .into_iter()
            .collect()
    );
}

#[test]
fn test_the_search_result_is_capped() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 23, 0, 0));
    for i in 0..40 {
        put(
            &t,
            "telegram",
            &format!("t-{i}"),
            "owner",
            "say",
            &format!("penguin fact {i} {}", "p".repeat(400)),
            at(21, 5, 0, 0) + Duration::minutes(i),
        );
    }
    let res = t.search_with(
        "penguin",
        &Search {
            limit: 50,
            ..Search::default()
        },
    );
    assert!(py::len(&py::dumps(&res)) <= SEARCH_RESULT_CHARS);
    let h = hits(&res);
    assert!(res["truncated"] == true && !h.is_empty());
    assert!(h
        .iter()
        .all(|x| py::len(x["text"].as_str().unwrap()) <= 300));
    assert!(h[0]["text"].as_str().unwrap().contains("penguin fact 39")); // newest first
}

#[test]
fn test_search_stops_at_its_time_budget() {
    let d = tmp();
    let (mut t, _) = store(d.path(), at(21, 23, 0, 0));
    for day in [18, 19, 20, 21] {
        put(&t, "voice", "v-1", "owner", "say", "owl", at(day, 9, 0, 0));
    }
    t.search_budget_secs = -1.0;
    assert_eq!(
        t.search("owl"),
        json!({"ok": true, "hits": [], "truncated": true})
    );
}

// ---- forget ---------------------------------------------------------------------------------------------------------

#[test]
fn test_match_is_the_shared_rule() {
    assert!(
        match_query("").is_none()
            && match_query("a b ?").is_none()
            && match_query("\"\"").is_none()
    );
    let pred = match_query("Blue \"red  car\"").unwrap();
    assert!(
        pred("a RED car and a blue one") && !pred("a red truck, blue") && !pred("red car only")
    );
    assert!(match_query("keys?").unwrap()("where are my keys")); // punctuation is not a term
}

#[test]
fn test_find_and_redact_keep_the_envelope_and_forget_the_words() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    put(
        &t,
        "voice",
        "v-1",
        "owner",
        "say",
        &format!("my {SENTINEL} code"),
        at(20, 9, 0, 0),
    );
    put(&t, "voice", "v-1", "eve", "say", "ok", at(20, 9, 1, 0));
    put(
        &t,
        "telegram",
        "t-1",
        "owner",
        "say",
        &format!("again {SENTINEL}"),
        at(21, 9, 0, 0),
    );
    t.close("telegram", "t-1", Some(at(21, 9, 5, 0)));
    let found = t.find(&SENTINEL.to_uppercase());
    assert_eq!(
        found
            .iter()
            .map(|f| (field(f, "day").to_string(), field(f, "ch").to_string()))
            .collect::<Vec<_>>(),
        [
            ("2026-09-20".to_string(), "voice".to_string()),
            ("2026-09-21".to_string(), "telegram".to_string())
        ]
    );
    assert!(!hits(&t.search(SENTINEL)).is_empty()); // cached before the forget
    let before = raw_lines(&t, "2026-09-20");
    assert_eq!(t.redact(SENTINEL, true), 3);
    assert_eq!(raw_lines(&t, "2026-09-20"), before); // counts, writes nothing
    assert_eq!(t.redact(SENTINEL, false), 3); // two lines and eve's answer to one
    let after = raw_lines(&t, "2026-09-20");
    for i in 0..2 {
        let mut want = before[i].clone();
        want.insert("text".into(), json!(FORGOTTEN));
        assert_eq!(after[i], want);
    }
    for day in ["2026-09-20", "2026-09-21"] {
        assert!(!fs::read_to_string(t.path_of(day))
            .unwrap()
            .contains(SENTINEL));
        assert_eq!(fs::metadata(t.path_of(day)).unwrap().mode() & 0o777, 0o600);
    }
    assert!(hits(&t.search(SENTINEL)).is_empty() && t.find(SENTINEL).is_empty()); // the cache did not keep it
    assert_eq!(t.redact(SENTINEL, false), 0);
    assert!(t.find("forgotten").is_empty()); // nothing matched twice
    assert_eq!(t.redact("x", false), 0);
    assert!(t.find("").is_empty()); // no usable term
    assert!(!fs::read_dir(&t.root)
        .unwrap()
        .flatten()
        .any(|e| e.file_name().to_string_lossy().ends_with(".tmp")));
}

#[test]
fn test_redact_drops_a_torn_line_holding_the_words() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    put(&t, "voice", "v-1", "owner", "say", "kept", at(21, 9, 0, 0));
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(t.path_of("2026-09-21"))
        .unwrap();
    writeln!(
        f,
        "{{\"ts\": \"2026-09-21T09:01:00\", \"text\": \"{SENTINEL} tor"
    )
    .unwrap();
    drop(f);
    put(
        &t,
        "voice",
        "v-1",
        "owner",
        "say",
        "also kept",
        at(21, 9, 2, 0),
    );
    assert_eq!(t.redact(SENTINEL, false), 1);
    assert!(!fs::read_to_string(t.path_of("2026-09-21"))
        .unwrap()
        .contains(SENTINEL));
    assert_eq!(texts(&t.lines("2026-09-21")), ["kept", "also kept"]);
}

#[test]
fn test_redact_loses_no_concurrent_append() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    for i in 0..2000 {
        let text = if i % 3 == 0 {
            format!("{SENTINEL} {i}")
        } else {
            format!("plain {i}")
        };
        t.append("voice", "v-1", "owner", "say", &text);
    }
    let stop = AtomicBool::new(false);
    let written = Mutex::new(Vec::<usize>::new());
    let removed = std::thread::scope(|s| {
        s.spawn(|| {
            let mut i = 0;
            while !stop.load(Ordering::SeqCst) || i < 300 {
                t.append("telegram", "t-1", "owner", "say", &format!("live {i}"));
                written.lock().push(i);
                i += 1;
            }
        });
        let removed = t.redact(SENTINEL, false);
        stop.store(true, Ordering::SeqCst);
        removed
    });
    let lines = t.lines("2026-09-21");
    let written = written.lock().len();
    assert_eq!(removed, 667);
    assert_eq!(
        lines
            .iter()
            .filter(|l| field(l, "text").starts_with("live "))
            .count(),
        written
    );
    assert_eq!(
        lines
            .iter()
            .filter(|l| field(l, "text") == FORGOTTEN)
            .count(),
        667
    );
    assert_eq!(lines.len(), 2000 + written);
}

// ---- beyond the Python file: the optional keywords and the readers other modules lean on ----------------------------

#[test]
fn test_append_writes_the_optional_keys_in_order_and_marks_outside_words() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    let att = vec![
        json!({"id": "a1", "name": "pic.jpg", "kind": "image", "extra": 1}),
        json!("not a dict"),
    ];
    let src = json!([{"kind": "said", "text": "the harbour", "when": "Tuesday"}, {"kind": "bogus", "text": "x"}]);
    assert!(t.append_with(
        "telegram",
        "t-1",
        "eve",
        "say",
        "an outside reply quoted from a web page the owner asked about",
        Append {
            now: Some(at(21, 9, 0, 0)),
            via: "app-door-that-is-long".into(),
            mid: "m-1".into(),
            att: Some(att),
            outside: true,
            src: Some(src),
            ..Append::default()
        }
    ));
    let raw = fs::read_to_string(t.path_of("2026-09-21")).unwrap();
    let first = raw.lines().next().unwrap();
    let line: Map<String, Value> = serde_json::from_str(first).unwrap();
    assert_eq!(
        line.keys().cloned().collect::<Vec<_>>(),
        ["ts", "ch", "conv", "who", "kind", "text", "via", "mid", "att", "out", "src"]
    );
    assert_eq!(line["via"], "app-door-that-is");
    assert_eq!(
        line["att"],
        json!([{"id": "a1", "name": "pic.jpg", "kind": "image"}])
    );
    assert_eq!(line["out"], 1);
    assert_eq!(
        line["src"],
        json!([{"kind": "said", "text": "the harbour", "when": "Tuesday"}])
    );
    assert!(first.starts_with("{\"ts\": \"2026-09-21T09:00:00")); // Python's separators
    assert!(eve_tool_gate::MARKS
        .found("prompt: an outside reply quoted from a web page the owner asked about, then more"));
    // a reply whose Sources quote forgotten words loses them and keeps its text
    assert_eq!(t.redact("harbour", false), 1);
    let line: Map<String, Value> = serde_json::from_str(
        fs::read_to_string(t.path_of("2026-09-21"))
            .unwrap()
            .lines()
            .next()
            .unwrap(),
    )
    .unwrap();
    assert!(
        line.get("src").is_none()
            && line["text"] == "an outside reply quoted from a web page the owner asked about"
    );
    assert_eq!(line.keys().next_back().unwrap(), "out"); // the other keys keep their places
}

#[test]
fn test_listeners_hear_each_day_and_a_panicking_one_costs_nothing() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    let heard = Arc::new(Mutex::new(Vec::<String>::new()));
    let h = heard.clone();
    t.add_listener(Arc::new(|_d: &str| panic!("listener down")));
    t.add_listener(Arc::new(move |d: &str| h.lock().push(d.to_string())));
    let prev = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let ok = put(&t, "voice", "v-1", "owner", "say", "hi", at(22, 3, 0, 0));
    std::panic::set_hook(prev);
    assert!(ok);
    assert_eq!(*heard.lock(), ["2026-09-21"]);
}

#[test]
fn test_bus_lines_are_searched_but_never_shown_as_talk() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 23, 0, 0));
    put(
        &t,
        BUS,
        "b-1",
        "owner",
        "say",
        "Tracker: 5 pushups",
        at(21, 9, 0, 0),
    );
    assert_eq!(t.today_block(4000), "");
    assert_eq!(t.last_turn_at(), None);
    assert_eq!(
        t.day_text("2026-09-21", 1000, false),
        "2026-09-21 09:00 sent Owner: Tracker: 5 pushups"
    );
    assert_eq!(hits(&t.search("pushups"))[0]["ch"], "bus");
}

#[test]
fn test_python_written_lines_load_and_a_naive_time_is_local() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    fs::create_dir_all(&t.root).unwrap();
    let mut f = fs::File::create(t.path_of("2026-09-21")).unwrap();
    // json.dumps(rec, ensure_ascii=False) as the Python wrote it, a naive stamp, and a line with no text field
    writeln!(f, "{{\"ts\": \"2026-09-21T09:00:00\", \"ch\": \"voice\", \"conv\": \"v-1\", \"who\": \"owner\", \"kind\": \"say\", \"text\": \"café\"}}").unwrap();
    writeln!(f, "{{\"ts\": \"2026-09-21T09:01:00\", \"ch\": \"voice\"}}").unwrap();
    writeln!(f, "[1, 2]").unwrap();
    drop(f);
    let lines = t.lines("2026-09-21");
    assert_eq!(texts(&lines), ["café"]);
    let ts = parse_ts(field(&lines[0], "ts")).unwrap();
    assert_eq!(ts.format("%H:%M").to_string(), "09:00");
    assert_eq!(render_line(&lines[0], "text"), "09:00 (spoken) Owner: café");
}

#[test]
fn test_odd_bytes_on_disk_are_read_as_python_reads_them() {
    let d = tmp();
    let (t, _) = store(d.path(), at(21, 12, 0, 0));
    put(&t, "voice", "v-1", "owner", "say", SENTINEL, at(21, 9, 0, 0));
    let mut f = fs::OpenOptions::new()
        .append(true)
        .open(t.path_of("2026-09-21"))
        .unwrap();
    // a vertical-tab-only chunk (bytes.strip() drops it on a rewrite) and a stamp with a letter where a digit
    // belongs (fromisoformat's ValueError: skipped, never a panic)
    f.write_all(b"\x0b\n").unwrap();
    writeln!(f, "{{\"ts\": \"2026-09-0\u{e9}\", \"ch\": \"voice\", \"conv\": \"v-1\", \"who\": \"owner\", \"kind\": \"say\", \"text\": \"odd\"}}").unwrap();
    drop(f);
    assert_eq!(texts(&t.lines("2026-09-21")), [SENTINEL]);
    assert_eq!(t.redact(SENTINEL, false), 1);
    let raw = fs::read(t.path_of("2026-09-21")).unwrap();
    assert!(!raw.contains(&0x0b));
    assert_eq!(texts(&t.lines("2026-09-21")), [FORGOTTEN]);
}

#[test]
fn test_conversation_ids_and_days_read_as_python_reads_them() {
    // a 16-digit id is past year 9999: fromtimestamp raises, so no birth time
    assert!(conv_epoch("v-9999999999999999-abcd").is_none());
    assert!(conv_epoch("v-1758450000000-abcd").is_some());
    // strptime and int() read any decimal digit
    assert_eq!(
        next_day("\u{662}\u{660}\u{662}\u{666}-\u{660}\u{669}-\u{663}\u{660}").as_deref(),
        Some("2026-10-01")
    );
    assert_eq!(next_day("2026-02-30"), None);
    assert_eq!(next_day("9999-12-31"), None);
}

#[test]
fn test_a_day_file_name_never_spans_a_newline() {
    // `.` in `_DAY_NOTE` stops at a newline, as Python's does without re.S
    let d = tmp();
    let archive = d.path().join("debrief");
    fs::create_dir_all(&archive).unwrap();
    fs::write(archive.join("2026-09-18-a\nb.md"), "- heron sighting\n").unwrap();
    fs::write(archive.join("2026-09-19-plain.md"), "- heron again\n").unwrap();
    let (t, _) = store_with(d.path(), at(21, 12, 0, 0), Some(archive));
    let got: Vec<String> = hits(&t.search("heron"))
        .iter()
        .map(|h| h["day"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(got, ["2026-09-19"]);
}
