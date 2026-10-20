//! The chat log's tests: the crate-level parts of bridge/tests/test_chat_memory.py (the file, its bounds, forget,
//! the switch, the seed), the SEED_KINDS assertion of test_app_media.py, and the file format byte for byte against
//! what chat_log.py writes.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{DateTime, Local};
use parking_lot::Mutex;
use serde_json::Value;

use eve_transcripts::{Append as TxAppend, TranscriptConfig, Transcripts};

use super::*;

/// test_telegram.NOW
const NOW: f64 = 1_800_000_000.0;
/// test_telegram.OWNER
const OWNER: i64 = 4242;

#[derive(Clone)]
struct Clock(Arc<Mutex<f64>>);

impl Clock {
    fn new(t: f64) -> Clock {
        Clock(Arc::new(Mutex::new(t)))
    }

    fn wall(&self) -> Wall {
        let c = self.0.clone();
        Arc::new(move || *c.lock())
    }

    fn add(&self, secs: f64) {
        *self.0.lock() += secs;
    }
}

fn new_log(path: &Path, clock: &Clock) -> ChatLog {
    ChatLog::with_options(path, Options { wall: Some(clock.wall()), ..Options::default() })
}

fn bounded_log(path: &Path, clock: &Clock, max_lines: usize, max_days: f64) -> ChatLog {
    ChatLog::with_options(path, Options { max_lines, max_days, wall: Some(clock.wall()) })
}

fn texts(lines: &[Line]) -> Vec<String> {
    lines.iter().map(|ln| ln.text.clone()).collect()
}

fn at(t: f64) -> Dt {
    DateTime::from_timestamp_micros((t * 1e6) as i64).unwrap().with_timezone(&Local).fixed_offset()
}

/// test_memory.build's transcripts: memory on, under the test's own folder (its home too, so no guard trips).
fn transcripts(tmp: &Path) -> Transcripts {
    let mut cfg = TranscriptConfig::new(true, tmp.join("memory").join("transcripts"));
    cfg.home = Some(tmp.to_path_buf());
    let t = Transcripts::new(cfg, None, None);
    assert!(t.enabled);
    t
}

fn tx_append(tx: &Transcripts, ch: &str, conv: &str, who: &str, text: &str, t: f64) {
    assert!(tx.append_with(ch, conv, who, "say", text, TxAppend { now: Some(at(t)), ..TxAppend::default() }));
}

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ---- forget, the file, the bounds (test_chat_memory.py) ----------------------------------------------------------

#[test]
fn test_forget_removes_lines_from_the_log() {
    let dir = tmp();
    let path = dir.path().join("chat").join("log.json");
    let clock = Clock::new(NOW);
    let log = new_log(&path, &clock);
    log.append(7, "user", "my locker code is a made-up zebra");
    log.append(7, "eve", "Got it.");
    // memory.forget_preview counts with count(), forget_apply drops with forget()
    assert_eq!(log.count(|t| t.contains("zebra")), 1);
    assert_eq!(log.forget(|t| t.contains("zebra")), 1);
    assert_eq!(texts(&log.lines(7)), ["Got it."]);
    assert!(!fs::read_to_string(&path).unwrap().contains("zebra"));
    // and a reload does not bring it back
    assert_eq!(texts(&new_log(&path, &clock).lines(7)), ["Got it."]);
}

#[test]
fn test_the_log_file_is_private_and_bounded() {
    let dir = tmp();
    let path = dir.path().join("chat").join("log.json");
    let clock = Clock::new(NOW);
    let log = bounded_log(&path, &clock, 5, 7.0);
    for i in 0..12 {
        log.append(1, if i % 2 == 0 { "user" } else { "eve" }, &format!("line {i}"));
    }
    log.append(2, "user", "another chat");
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
    assert_eq!(fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777, 0o700);
    let want: Vec<String> = (7..12).map(|i| format!("line {i}")).collect();
    assert_eq!(texts(&log.lines(1)), want);
    let reloaded = bounded_log(&path, &clock, 5, 7.0);
    assert_eq!(texts(&reloaded.lines(1)), want);
    assert_eq!(texts(&reloaded.lines(2)), ["another chat"]);
    clock.add(8.0 * 86400.0); // past the age bound
    assert!(reloaded.lines(1).is_empty());
    reloaded.append(1, "user", "new");
    let doc: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    let on_disk: Vec<&str> = doc["lines"].as_array().unwrap().iter().map(|r| r["text"].as_str().unwrap()).collect();
    assert_eq!(on_disk, ["new"]);
    let tmps = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().ends_with(".tmp"))
        .count();
    assert_eq!(tmps, 0);
}

#[test]
fn test_a_torn_file_starts_empty_and_never_crashes() {
    let dir = tmp();
    let path = dir.path().join("chat").join("log.json");
    fs::create_dir(path.parent().unwrap()).unwrap();
    fs::set_permissions(path.parent().unwrap(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(&path, r#"{"version": 1, "lines": [{"seq": 1"#).unwrap();
    let log = new_log(&path, &Clock::new(NOW));
    assert!(log.lines(1).is_empty());
    log.append(1, "user", "fresh");
    assert_eq!(texts(&new_log(&path, &Clock::new(NOW)).lines(1)), ["fresh"]);
}

#[test]
fn test_a_forget_by_another_process_is_not_undone_by_the_next_write() {
    let dir = tmp();
    let path = dir.path().join("chat").join("log.json");
    let clock = Clock::new(NOW);
    let daemon = new_log(&path, &clock);
    daemon.append(7, "user", "my locker code is a made-up zebra");
    daemon.append(7, "eve", "Got it.");
    let cli = new_log(&path, &clock); // the CLI's forget, while the daemon runs
    assert_eq!(cli.forget(|t| t.contains("zebra")), 1);
    daemon.append(7, "user", "hello"); // the daemon's next write
    assert!(!fs::read_to_string(&path).unwrap().contains("zebra"));
    let lines = daemon.lines(7);
    assert_eq!(texts(&lines), ["Got it.", "hello"]);
    let seqs: std::collections::HashSet<i64> = lines.iter().map(|ln| ln.seq).collect();
    assert_eq!(seqs.len(), 2);
    // the number the other process dropped is not reused either
    assert_eq!(lines[1].seq, 3);
}

// ---- the switch ----------------------------------------------------------------------------------------------------

#[test]
fn test_the_switch_off_restores_the_old_behaviour() {
    let dir = tmp();
    assert!(configured(Some(&env(&[]))).unwrap().enabled); // on by default
    let file = dir.path().join("x.json");
    for off in ["0", "off", "false", "no", " OFF "] {
        let e = env(&[("EVE_CHAT_LOG", off), ("EVE_CHAT_LOG_FILE", file.to_str().unwrap())]);
        assert!(open_configured(Some(&e)).unwrap().is_none());
    }
    assert!(!file.exists());
    // an empty or unknown value is on
    for on in ["", "  ", "1", "yes", "maybe"] {
        assert!(configured(Some(&env(&[("EVE_CHAT_LOG", on)]))).unwrap().enabled, "{on:?}");
    }
}

#[test]
fn test_the_daemon_opens_the_file_the_environment_names() {
    // test_the_daemon_lends_the_log_to_the_door_and_to_forget: str(log.path) is EVE_CHAT_LOG_FILE as given
    let dir = tmp();
    let file = dir.path().join("chat").join("log.json");
    let e = env(&[("EVE_CHAT_LOG_FILE", file.to_str().unwrap())]);
    let log = open_configured(Some(&e)).unwrap().unwrap();
    assert_eq!(log.path, file);
    assert_eq!(log.path.to_str(), e.get("EVE_CHAT_LOG_FILE").map(String::as_str));
    assert_eq!(log.max_lines, MAX_LINES);
    assert_eq!(log.max_days, MAX_DAYS);
}

#[test]
fn test_configured_reads_the_bounds_as_python_does() {
    let cfg = |pairs: &[(&str, &str)]| configured(Some(&env(pairs)));
    let d = cfg(&[]).unwrap();
    assert_eq!((d.max_lines, d.max_days), (MAX_LINES, MAX_DAYS));
    assert_eq!(d.path, tpy::expand_user(Path::new(DEFAULT_FILE)));
    let c = cfg(&[("EVE_CHAT_LOG_LINES", " 50 "), ("EVE_CHAT_LOG_DAYS", "1.5")]).unwrap();
    assert_eq!((c.max_lines, c.max_days), (50, 1.5));
    // int(float(...)): cut toward zero; underscores and Unicode digits as float() takes them
    assert_eq!(cfg(&[("EVE_CHAT_LOG_LINES", "12.9")]).unwrap().max_lines, 12);
    assert_eq!(cfg(&[("EVE_CHAT_LOG_LINES", "1_000")]).unwrap().max_lines, 1000);
    assert_eq!(cfg(&[("EVE_CHAT_LOG_LINES", "١٢")]).unwrap().max_lines, 12);
    assert_eq!(cfg(&[("EVE_CHAT_LOG_LINES", "1e2")]).unwrap().max_lines, 100);
    // below the floor, unreadable or NaN: the default
    for bad in ["0", "0.5", "-3", "lots", "", "nan", "1__0", "_1", "+-1"] {
        assert_eq!(cfg(&[("EVE_CHAT_LOG_LINES", bad)]).unwrap().max_lines, MAX_LINES, "{bad:?}");
    }
    assert_eq!(cfg(&[("EVE_CHAT_LOG_DAYS", "0.001")]).unwrap().max_days, MAX_DAYS);
    assert_eq!(cfg(&[("EVE_CHAT_LOG_DAYS", "0.01")]).unwrap().max_days, 0.01);
    assert_eq!(cfg(&[("EVE_CHAT_LOG_DAYS", "inf")]).unwrap().max_days, f64::INFINITY);
    // int(float("inf")) raises OverflowError in Python, out of configured and open_configured
    assert_eq!(cfg(&[("EVE_CHAT_LOG_LINES", "Infinity")]), Err(ConfigError::Overflow));
    assert_eq!(cfg(&[("EVE_CHAT_LOG_LINES", "1e400")]), Err(ConfigError::Overflow));
    // the file: stripped, ~ expanded, spelled as pathlib spells it
    assert_eq!(cfg(&[("EVE_CHAT_LOG_FILE", "  /a//b/./c.json/ ")]).unwrap().path, PathBuf::from("/a/b/c.json"));
    assert_eq!(cfg(&[("EVE_CHAT_LOG_FILE", "   ")]).unwrap().path, d.path);
    assert_eq!(cfg(&[("EVE_CHAT_LOG_FILE", "~/x.json")]).unwrap().path, tpy::home().join("x.json"));
}

// ---- the seed (test_chat_memory.py, the live failure of 2026-10-08 evening) -----------------------------------------

const LOCKER: &str = "(memory test, just reply ok) my locker code is 4417";
const EARLIER: &str = "remind me to buy oat milk on the way home";

/// Talk from before the chat log existed: one Telegram exchange, and a voice line that is not this chat's.
fn earlier_talk(tx: &Transcripts, conv: &str) {
    tx_append(tx, "voice", "v-1799997000000-beef", "owner", "play some made-up jazz", NOW - 3000.0);
    tx_append(tx, "telegram", conv, "owner", EARLIER, NOW - 1800.0);
    tx_append(tx, "telegram", conv, "eve", "Will do: oat milk.", NOW - 1790.0);
}

#[test]
fn test_the_seed_runs_once_and_forget_reaches_the_seeded_copy() {
    let dir = tmp();
    let tx = transcripts(dir.path());
    tx_append(&tx, "telegram", "t-1799998000000-abcd", "owner", "my made-up zebra code is 9", NOW - 600.0);
    let path = dir.path().join("chat").join("log.json");
    let clock = Clock::new(NOW);
    let log = new_log(&path, &clock);
    assert_eq!(log.seed(OWNER, Some(&tx)), 1);
    assert_eq!(log.seed(OWNER, Some(&tx)), 0); // marked: never twice
    assert_eq!(new_log(&path, &clock).seed(OWNER, Some(&tx)), 0); // not even after a restart
    assert_eq!(log.count(|t| t.contains("zebra")), 1);
    assert_eq!(log.forget(|t| t.contains("zebra")), 1);
    assert!(!fs::read_to_string(&path).unwrap().contains("zebra"));
    // after the forget emptied the chat it is still marked
    assert_eq!(log.seed(OWNER, Some(&tx)), 0);
}

#[test]
fn test_the_seed_moves_a_quote_with_the_line_it_points_at() {
    // Review D7: the seed renumbers every kept line; an eve line's quotes must follow its owner line.
    let dir = tmp();
    let tx = transcripts(dir.path());
    tx_append(&tx, "telegram", "t-1799998000000-abcd", "owner", "an older made-up line", NOW - 600.0);
    let log = new_log(&dir.path().join("chat").join("log.json"), &Clock::new(NOW));
    let say = |conv: &str, at: f64, quotes: i64| Append {
        kind: "say".into(),
        conv: conv.into(),
        at: Some(at),
        quotes,
        ..Append::default()
    };
    let asked = log.append_with(OWNER, "user", "what did I say?", say("t-live", NOW - 100.0, 0)).unwrap();
    log.append_with(OWNER, "eve", "You said that.", say("t-live", NOW - 90.0, asked.seq));
    let before = log.generation();
    assert_eq!(log.seed(OWNER, Some(&tx)), 1);
    assert!(log.generation() > before);
    let lines = log.lines(OWNER);
    let owner = lines.iter().find(|ln| ln.text == "what did I say?").unwrap();
    let eve = lines.iter().find(|ln| ln.text == "You said that.").unwrap();
    assert_eq!(eve.quotes, owner.seq);
    let seeded = &lines[0];
    assert_eq!((seeded.seq, seeded.text.as_str(), seeded.boot.as_str()), (1, "an older made-up line", SEED_BOOT));
    assert_eq!((seeded.who.as_str(), seeded.kind.as_str()), ("user", "say"));
    assert_eq!(seeded.conv, "t-1799998000000-abcd");
    // the next line is numbered after every one
    assert_eq!(log.append(OWNER, "user", "next").unwrap().seq, 4);
}

#[test]
fn test_live_before_that_with_memory_on_and_a_brand_new_log() {
    // the seed part: the talk from before the log's first boot is there, in order, before the newest line; the
    // voice line is not
    let dir = tmp();
    let tx = transcripts(dir.path());
    earlier_talk(&tx, "t-1799998000000-abcd");
    let clock = Clock::new(NOW);
    let log = new_log(&dir.path().join("chat").join("log.json"), &clock);
    assert_eq!(log.seed(OWNER, Some(&tx)), 2);
    log.append_with(OWNER, "user", LOCKER, Append { kind: "say".into(), conv: "t-now".into(), ..Append::default() });
    assert_eq!(texts(&log.lines(OWNER)), [EARLIER, "Will do: oat milk.", LOCKER]);
    assert!(log.lines(OWNER).iter().all(|ln| !ln.text.contains("jazz")));
    assert_eq!(log.lines(OWNER)[1].who, "eve");
}

#[test]
fn test_live_before_that_with_memory_on_and_the_log_already_full() {
    // nothing is seeded twice: the transcripts' copy of the earlier exchange is the log's own
    let dir = tmp();
    let tx = transcripts(dir.path());
    let conv = "t-1799998000000-abcd";
    earlier_talk(&tx, conv);
    let path = dir.path().join("chat").join("log.json");
    let clock = Clock::new(NOW);
    let old = new_log(&path, &clock); // an earlier run of the daemon wrote these
    let say = |at: f64| Append { kind: "say".into(), conv: conv.into(), at: Some(at), ..Append::default() };
    old.append_with(OWNER, "user", EARLIER, say(NOW - 1800.0));
    old.append_with(OWNER, "eve", "Will do: oat milk.", say(NOW - 1790.0));
    let log = new_log(&path, &clock);
    assert_eq!(log.seed(OWNER, Some(&tx)), 0);
    assert_eq!(texts(&log.lines(OWNER)).iter().filter(|t| *t == EARLIER).count(), 1);
}

#[test]
fn test_the_seed_keeps_to_its_bounds() {
    let dir = tmp();
    let tx = transcripts(dir.path());
    // too old, another chat's channel, a forgotten line, a kind the history does not show
    tx_append(&tx, "telegram", "t-1799000000000-aaaa", "owner", "eight days ago", NOW - 8.0 * 86400.0);
    tx_append(&tx, "voice", "v-1799998000000-bbbb", "owner", "a voice line", NOW - 500.0);
    tx_append(&tx, "telegram", "t-1799998000000-cccc", "owner", "fresh enough", NOW - 400.0);
    tx_append(&tx, "telegram", "t-1799998000000-cccc", "eve", "(forgotten)", NOW - 390.0);
    assert!(tx.append_with(
        "telegram",
        "t-1799998000000-cccc",
        "eve",
        "tool",
        "a tool line",
        TxAppend { now: Some(at(NOW - 380.0)), tool: "web_search".into(), ..TxAppend::default() }
    ));
    let log = new_log(&dir.path().join("chat").join("log.json"), &Clock::new(NOW));
    // a newer line of the chat: only what is older than it goes in
    log.append_with(OWNER, "user", "kept", Append { at: Some(NOW - 450.0), ..Append::default() });
    assert_eq!(log.seed(OWNER, Some(&tx)), 0);
    let log2 = new_log(&dir.path().join("chat2").join("log.json"), &Clock::new(NOW));
    assert_eq!(log2.seed(OWNER, Some(&tx)), 1);
    assert_eq!(texts(&log2.lines(OWNER)), ["fresh enough"]);
    // memory off: nothing, and the chat is not marked
    let off = Transcripts::new(TranscriptConfig::new(false, dir.path().join("off")), None, None);
    let log3 = new_log(&dir.path().join("chat3").join("log.json"), &Clock::new(NOW));
    assert_eq!(log3.seed(OWNER, Some(&off)), 0);
    assert_eq!(log3.seed(OWNER, None), 0);
    assert_eq!(log3.seed(OWNER, Some(&tx)), 1);
}

#[test]
fn test_the_seed_takes_the_newest_that_fit_with_pythons_slice() {
    // room = max_lines - the chat's lines; picked[len - room:] keeps Python's negative-start slice
    assert_eq!(py_tail(vec![1, 2, 3], 0), Vec::<i32>::new());
    assert_eq!(py_tail(vec![1, 2, 3], 2), [2, 3]);
    assert_eq!(py_tail(vec![1, 2, 3], 3), [1, 2, 3]);
    assert_eq!(py_tail(vec![1, 2, 3], 4), [3]); // [1, 2, 3][-1:]
    assert_eq!(py_tail(vec![1, 2, 3], 5), [2, 3]); // [1, 2, 3][-2:]
    assert_eq!(py_tail(vec![1, 2, 3], 6), [1, 2, 3]);
    assert_eq!(py_tail(vec![1, 2, 3], 300), [1, 2, 3]);
    let dir = tmp();
    let tx = transcripts(dir.path());
    for i in 0..4 {
        let t = NOW - 1000.0 + f64::from(i) * 10.0;
        tx_append(&tx, "telegram", "t-1799998000000-abcd", "owner", &format!("old {i}"), t);
    }
    let log = bounded_log(&dir.path().join("chat").join("log.json"), &Clock::new(NOW), 3, 7.0);
    log.append(OWNER, "user", "now");
    // room 2 of 4 rows: the newest two
    assert_eq!(log.seed(OWNER, Some(&tx)), 2);
    assert_eq!(texts(&log.lines(OWNER)), ["old 2", "old 3", "now"]);
}

#[test]
fn test_the_seed_never_fails_on_a_clock_it_cannot_hold() {
    let dir = tmp();
    let tx = transcripts(dir.path());
    tx_append(&tx, "telegram", "t-1799998000000-abcd", "owner", "a line", NOW - 60.0);
    let log = bounded_log(&dir.path().join("chat").join("log.json"), &Clock::new(NOW), 300, f64::INFINITY);
    // datetime.fromtimestamp(-inf) raises: the seed is skipped and the chat not marked
    assert_eq!(log.seed(OWNER, Some(&tx)), 0);
    assert!(local_moment(f64::NEG_INFINITY).is_err());
    assert!(local_moment(-1e15).is_err());
    assert!(local_moment(NOW).is_ok());
}

// ---- writing --------------------------------------------------------------------------------------------------------

#[test]
fn test_a_note_with_the_same_key_replaces_the_newest_line() {
    let dir = tmp();
    let clock = Clock::new(NOW);
    let log = new_log(&dir.path().join("chat").join("log.json"), &clock);
    let note = |key: &str, outside: bool| Append { key: key.into(), outside, ..Append::default() };
    let a = log.append_with(1, "event", "Codex answered: one", note("codex", true)).unwrap();
    clock.add(5.0);
    let b = log.append_with(1, "event", "Codex answered: two", note("codex", false)).unwrap();
    assert_eq!(a.seq, b.seq);
    assert!(b.outside); // once outside, always outside
    assert_eq!(b.t, NOW + 5.0);
    assert_eq!(texts(&log.lines(1)), ["Codex answered: two"]);
    // another chat's newest line is not this chat's
    log.append_with(2, "event", "Codex answered: three", note("codex", false));
    assert_eq!(log.lines(1).len(), 1);
    // a line in between ends the run
    log.append(1, "user", "thanks");
    log.append_with(1, "event", "Codex answered: four", note("codex", false));
    assert_eq!(texts(&log.lines(1)), ["Codex answered: two", "thanks", "Codex answered: four"]);
    // an empty key never replaces
    log.append(1, "eve", "a");
    log.append(1, "eve", "b");
    assert_eq!(log.lines(1).len(), 5);
}

#[test]
fn test_append_refuses_an_unknown_speaker_and_empty_text_and_cuts_long_text() {
    let dir = tmp();
    let log = new_log(&dir.path().join("chat").join("log.json"), &Clock::new(NOW));
    assert!(log.append(1, "owner", "hi").is_none());
    assert!(log.append(1, "user", " \n\u{2028} ").is_none());
    let long = "é".repeat(TEXT_CHARS + 10);
    let kept = log.append(1, "user", &long).unwrap();
    assert_eq!(kept.text.chars().count(), TEXT_CHARS);
    assert!(kept.text.ends_with('…'));
    let exact = "x".repeat(TEXT_CHARS);
    assert_eq!(log.append(1, "user", &exact).unwrap().text, exact);
}

#[test]
fn test_clear_empties_one_chat_or_all() {
    let dir = tmp();
    let log = new_log(&dir.path().join("chat").join("log.json"), &Clock::new(NOW));
    log.append(1, "user", "a");
    log.append(2, "user", "b");
    log.append(2, "eve", "c");
    let g = log.generation();
    assert_eq!(log.clear(Some(3)), 0);
    assert_eq!(log.generation(), g);
    assert_eq!(log.clear(Some(2)), 2);
    assert!(log.generation() > g);
    assert_eq!(texts(&log.lines(1)), ["a"]);
    assert_eq!(log.clear(None), 1);
    assert!(log.lines(1).is_empty());
}

#[test]
fn test_outside_words_are_marked_again_when_the_file_is_read() {
    let dir = tmp();
    let path = dir.path().join("chat").join("log.json");
    let clock = Clock::new(NOW);
    let words = "a made-up sentence the agent wrote, long enough to fingerprint 7731";
    new_log(&path, &clock).append_with(1, "eve", words, Append { outside: true, ..Append::default() });
    eve_tool_gate::MARKS.clear();
    let log = new_log(&path, &clock);
    assert!(log.lines(1)[0].outside);
    assert!(eve_tool_gate::MARKS.found(&format!("history: {words}")));
}

#[test]
fn test_a_refused_folder_keeps_the_log_in_ram_only() {
    let dir = tmp();
    let repo = dir.path().join("repo");
    fs::create_dir_all(repo.join(".git")).unwrap();
    fs::create_dir_all(repo.join("chat")).unwrap();
    let path = repo.join("chat").join("log.json");
    let log = new_log(&path, &Clock::new(NOW));
    assert!(log.refused().contains("git work tree"));
    log.append(1, "user", "kept in RAM");
    assert_eq!(texts(&log.lines(1)), ["kept in RAM"]);
    assert!(!path.exists());
    // a folder that does not exist yet is checked when the first write makes it
    let later = repo.join("later").join("log.json");
    let log = new_log(&later, &Clock::new(NOW));
    assert_eq!(log.refused(), "");
    log.append(1, "user", "x");
    assert!(log.refused().contains("git work tree"));
    assert!(!later.parent().unwrap().exists());
}

// ---- the file format --------------------------------------------------------------------------------------------

/// What chat_log.py wrote for the same calls (boot "abcd1234", wall 1790000000.25, chats 7 and -3 marked seeded).
const PYTHON_FILE: &str = concat!(
    r#"{"version":1,"seeded":[-3,7],"lines":[{"seq":1,"t":1790000000.25,"chat":5,"who":"user","#,
    r#""text":"héllo \"q\" \\ tab\there\u0001","kind":"say","conv":"t-1","boot":"abcd1234","key":"","quotes":0,"#,
    r#""outside":false,"extra":{}},{"seq":2,"t":1790000000.25,"chat":5,"who":"event","text":"Codex answered: two","#,
    r#""kind":"","conv":"","boot":"abcd1234","key":"codex","quotes":0,"outside":true,"extra":{}}]}"#
);

#[test]
fn test_the_file_is_byte_for_byte_what_python_writes() {
    let dir = tmp();
    let path = dir.path().join("chat").join("log.json");
    let mut log = new_log(&path, &Clock::new(1_790_000_000.25));
    log.boot = "abcd1234".into();
    log.append_with(
        5,
        "user",
        "  héllo \"q\" \\ tab\there\u{1} \u{2028} ",
        Append { kind: "say".into(), conv: "t-1".into(), ..Append::default() },
    );
    log.append_with(
        5,
        "event",
        "Codex answered: one",
        Append { key: "codex".into(), at: Some(1_790_000_001.0), ..Append::default() },
    );
    log.append_with(5, "event", "Codex answered: two", Append { key: "codex".into(), outside: true, ..Append::default() });
    log.append_with(-100_123, "eve", "ok", Append { quotes: 1, at: Some(1e9), ..Append::default() });
    {
        let mut st = log.state.lock();
        st.seeded.insert(7);
        st.seeded.insert(-3);
        log.save(&mut st);
    }
    assert_eq!(fs::read_to_string(&path).unwrap(), PYTHON_FILE);
    // and the Rust log reads the Python file back whole
    let back = new_log(&path, &Clock::new(1_790_000_000.25));
    let lines = back.lines(5);
    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].text, "héllo \"q\" \\ tab\there\u{1}");
    assert!(back.state.lock().seeded.contains(&-3));
    assert_eq!(back.seed(7, None), 0);
}

#[test]
fn test_rows_are_read_as_python_reads_them() {
    let dir = tmp();
    let path = dir.path().join("chat").join("log.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let rows = serde_json::json!({
        "version": 1,
        "seeded": [3, true, 4.0, "5", 6],
        "lines": [
            {"seq": "4", "t": "1800000000", "chat": 1, "who": "user", "text": "string numbers"},
            {"seq": 2.9, "t": NOW, "chat": 1.5, "who": "eve", "text": 12, "kind": 0, "conv": null, "quotes": "1"},
            {"seq": 3, "t": NOW, "chat": 1, "who": "owner", "text": "unknown speaker"},
            {"seq": 5, "t": NOW, "chat": 1, "who": "user", "text": ""},
            {"seq": 6, "t": NOW, "chat": 1, "who": "user"},
            {"seq": null, "t": NOW, "chat": 1, "who": "user", "text": "no seq"},
            {"seq": 7, "t": NOW, "chat": [1], "who": "user", "text": "a list chat"},
            {"seq": 8, "t": "soon", "chat": 1, "who": "user", "text": "a word time"},
            {"seq": 9, "t": NOW, "chat": 1, "who": "user", "text": "bad quote", "quotes": "x"},
            {"seq": 1, "t": true, "chat": 1, "who": "user", "text": "too old", "outside": 1},
            "not a row",
            {"seq": 10, "t": NOW, "chat": true, "who": "event", "text": "bool chat", "key": "k", "outside": "yes"}
        ]
    });
    fs::write(&path, rows.to_string()).unwrap();
    let log = new_log(&path, &Clock::new(NOW));
    {
        let st = log.state.lock();
        assert_eq!(st.seeded.iter().copied().collect::<Vec<_>>(), [3, 6]);
        assert_eq!(st.seq, 10);
    }
    let lines = log.lines(1);
    let got: Vec<(i64, &str, &str, i64, bool)> =
        lines.iter().map(|l| (l.seq, l.who.as_str(), l.text.as_str(), l.quotes, l.outside)).collect();
    assert_eq!(
        got,
        [(2, "eve", "12", 1, false), (4, "user", "string numbers", 0, false), (10, "event", "bool chat", 0, true)]
    );
    assert_eq!(lines[0].kind, "");
    assert_eq!(lines[0].conv, "");
    assert_eq!(lines[2].key, "k");
    // not a JSON object at all: empty
    fs::write(&path, "[1, 2]").unwrap();
    assert!(new_log(&path, &Clock::new(NOW)).lines(1).is_empty());
}

#[test]
fn test_python_number_parsing() {
    assert_eq!(py::float_str(" 1_000.5 "), Some(1000.5));
    assert_eq!(py::float_str("-InFiNiTy"), Some(f64::NEG_INFINITY));
    assert!(py::float_str("nan").unwrap().is_nan());
    assert_eq!(py::float_str(".5"), Some(0.5));
    assert_eq!(py::float_str("5."), Some(5.0));
    assert_eq!(py::float_str("1e1_0"), Some(1e10));
    for bad in ["", " ", "1_", "_1", "1__0", "1e", "e5", "--1", "+-1", "0x10", "None", "1 2"] {
        assert_eq!(py::float_str(bad), None, "{bad:?}");
    }
    assert_eq!(py::int_str(" -1_2 "), Some(-12));
    assert_eq!(py::int_str("٣"), Some(3));
    assert_eq!(py::int_str("007"), Some(7));
    for bad in ["", "1.0", "1e3", "+", "1_", "99999999999999999999"] {
        assert_eq!(py::int_str(bad), None, "{bad:?}");
    }
    assert_eq!(py::int_value(&serde_json::json!(-2.7)), Some(-2));
    assert_eq!(py::int_value(&serde_json::json!(false)), Some(0));
    assert_eq!(py::int_value(&Value::Null), None);
}

#[test]
fn test_note_text_takes_links_out_and_caps_the_line() {
    assert_eq!(note_text("see https://x.y/z and www.a.b\n now  ", 10), "see (link…");
    assert_eq!(note_text("abc", 0), "ab…"); // flat[:-1], as Python slices a cap of 0
    assert_eq!(note_text("ab", 2), "ab");
    assert_eq!(note_text("fix the tests at HTTPS://example.com/ci/1\tplease", NOTE_CHARS), "fix the tests at (link) please");
    assert_eq!(note_text("a\u{1c}b", NOTE_CHARS), "a b");
    assert_eq!(note_text("", NOTE_CHARS), "");
    let long = "word ".repeat(100);
    let n = note_text(&long, NOTE_CHARS);
    assert_eq!(n.chars().count(), NOTE_CHARS);
    assert!(n.ends_with("word…"));
}

// ---- test_app_media.py --------------------------------------------------------------------------------------------

#[test]
fn test_media_is_not_a_kind_the_seed_copies() {
    assert!(!SEED_KINDS.contains(&"media"));
    assert!(!eve_transcripts::transcripts::RENDER_KINDS.contains(&"media"));
}

#[test]
fn test_a_time_python_wrote_is_written_back_with_the_same_digits() {
    // serde_json's default float reader is off by one unit in the last place for some reprs; float_roundtrip is on
    let dir = tmp();
    let path = dir.path().join("chat").join("log.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let python = concat!(
        r#"{"version":1,"seeded":[],"lines":[{"seq":9,"t":1790925323.345832,"chat":-5,"who":"eve","text":"word","#,
        r#""kind":"","conv":"t-1","boot":"b00710c0","key":"","quotes":0,"outside":false,"extra":{}}]}"#
    );
    fs::write(&path, python).unwrap();
    let log = new_log(&path, &Clock::new(1_790_925_400.0));
    assert_eq!(log.lines(-5)[0].t, 1_790_925_323.345_832);
    log.save(&mut log.state.lock());
    assert_eq!(fs::read_to_string(&path).unwrap(), python);
}
