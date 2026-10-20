//! The forget tests of test_memory_records.py (forget follows a fact's src, switch on or off) and
//! test_forget_reworded.py (forget takes the dream's own wording of a forgotten line), on the real records.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use chrono::{Local, NaiveDate, TimeZone};
use serde_json::json;

use eve_records::{load_records, RecallConfig, Seams};
use eve_transcripts::transcripts::{Append, Dt, TranscriptConfig, Transcripts, FORGOTTEN};

use crate::memory::{reworded, Memory, MemoryOptions};
use crate::testutil::{at_tz, no_env, read, tmp, tz_on};

fn cfg_at(tmp: &Path) -> RecallConfig {
    let store = tmp.join("memory");
    std::fs::create_dir_all(&store).unwrap();
    RecallConfig::new(store, tmp.join("notes"))
}

fn put(cfg: &RecallConfig, name: &str, text: &str) {
    std::fs::create_dir_all(cfg.records_dir()).unwrap();
    std::fs::write(cfg.records_dir().join(name), text).unwrap();
}

fn memory_over(cfg: &RecallConfig, tx: Arc<Transcripts>) -> Memory {
    Memory::with_options(cfg.clone(), tx, None, MemoryOptions { environ: no_env(), ..MemoryOptions::default() })
}

fn transcripts_at(cfg: &RecallConfig, wall: Dt) -> Arc<Transcripts> {
    Arc::new(Transcripts::new(
        TranscriptConfig::new(true, cfg.transcripts_dir()),
        Some(cfg.archive_dir()),
        Some(Arc::new(move || wall)),
    ))
}

fn append(tx: &Transcripts, ch: &str, conv: &str, who: &str, text: &str, when: Dt) {
    assert!(tx.append_with(ch, conv, who, "say", text, Append { now: Some(when), ..Append::default() }));
}

// ---- test_memory_records.py: forget follows src ----------------------------------------------------------

fn src_memory(tmp: &Path) -> (Memory, RecallConfig) {
    let cfg = cfg_at(tmp);
    let naive = NaiveDate::from_ymd_opt(2026, 9, 20).unwrap().and_hms_opt(12, 1, 0).unwrap();
    let when = Local.from_local_datetime(&naive).earliest().unwrap().fixed_offset();
    let tx = transcripts_at(&cfg, when);
    append(&tx, "telegram", "t-1789930000000-abcd", "owner", "my locker code is 3141", when);
    let stamp = when.format("%Y-%m-%d %H:%M");
    put(
        &cfg,
        "gym.md",
        &format!("---\nid: gym\ntype: place\naliases: [gym]\n---\n- Keeps a combination at the gym. {{src: {stamp}}}\n- Goes on Mondays.\n"),
    );
    (memory_over(&cfg, tx), cfg)
}

#[test]
fn test_forget_follows_the_src_pointer_to_a_fact_in_other_words() {
    let d = tmp();
    let env: HashMap<String, String> = [(eve_records::records::SOURCES_ENV.to_string(), "1".to_string())].into();
    eve_records::with_seams(Seams { env: Some(env), ..Seams::default() }, || {
        let (m, cfg) = src_memory(d.path());
        let prev = m.forget_preview("locker code 3141");
        assert!(prev["layers"]["transcripts"] == json!(1) && prev["layers"]["records"] == json!(1));
        let done = m.forget_apply(prev["token"].as_str().unwrap());
        assert!(done["ok"] == json!(true) && done["layers"]["records"] == json!(1));
        assert_eq!(load_records(&cfg)["gym"].facts, ["Goes on Mondays."]);
    });
}

#[test]
fn test_with_sources_off_forget_still_follows_a_src_already_written() {
    let d = tmp();
    let (m, cfg) = src_memory(d.path()); // a fact written while the switch was on
    let prev = m.forget_preview("locker code 3141");
    assert_eq!(prev["layers"]["records"], json!(1));
    m.forget_apply(prev["token"].as_str().unwrap());
    assert_eq!(load_records(&cfg)["gym"].facts, ["Goes on Mondays."]); // control: the fact with no src stays
}

#[test]
fn test_mem0_search_shows_src() {
    // the Memory half of test_mem0_search_shows_src_and_a_forget_reaches_a_memory_by_its_src
    let row = json!({"text": "Lives on Quarry Street", "day": "2026-09-20", "channel": "telegram", "src": "2026-09-20 12:00"});
    assert_eq!(Memory::recalled(&row), "(2026-09-20 12:00, telegram) Lives on Quarry Street");
    assert_eq!(Memory::recalled(&json!({"text": "Likes tea", "day": "2026-09-20"})), "(2026-09-20) Likes tea");
    assert_eq!(Memory::recalled(&json!({"text": "Likes tea"})), "Likes tea");
    assert_eq!(Memory::recalled(&json!("a plain row")), "a plain row");
}

// ---- test_forget_reworded.py -------------------------------------------------------------------------

const SAID: &str = "Honestly I failed my driving test back in July.";

fn at(hh: u32, mm: u32) -> Dt {
    at_tz(tz_on(2026, 8, 14), 2026, 8, 14, hh, mm)
}

fn build(tmp: &Path) -> (Memory, Arc<Transcripts>, RecallConfig) {
    let cfg = RecallConfig::new(tmp.join("memory"), tmp.join("notes"));
    let tx = transcripts_at(&cfg, at(23, 0));
    (memory_over(&cfg, tx.clone()), tx, cfg)
}

fn day_texts(tx: &Transcripts) -> Vec<String> {
    tx.lines("2026-08-14").iter().map(|ln| ln["text"].as_str().unwrap().to_string()).collect()
}

#[test]
fn test_reworded_takes_the_dreams_words_and_leaves_what_shares_one_word() {
    let pred = reworded(SAID).unwrap();
    assert!(pred("- Avoid driving for now due to failing the driving test in July (active 2026-08-14)."));
    assert!(!pred("- 2026-08-14 20:43 — Owner confirmed/accepted: no driving."));
    assert!(!pred("Uncle offered to let them drive his car."));
    let counselor = reworded("I've started seeing a counselor on Tuesdays at 5.").unwrap();
    assert!(counselor("- counseling-tuesday-5 (routine): counselor, counseling, therapy"));
    assert!(!counselor("- Considering intramural volleyball on Tuesday nights."));
}

#[test]
fn test_reworded_is_off_for_a_short_or_quoted_query() {
    assert!(reworded("failed").is_none());
    assert!(reworded("driving test").is_none());
    assert!(reworded(&format!("\"{SAID}\"")).is_none());
    assert!(reworded("").is_none());
}

#[test]
fn test_forget_takes_eves_answer_and_nothing_after_it() {
    let d = tmp();
    let (_, tx, _) = build(d.path());
    append(&tx, "voice", "v-1", "owner", SAID, at(20, 41));
    append(&tx, "voice", "v-1", "eve", "Okay, no driving then.", at(20, 42));
    append(&tx, "voice", "v-1", "owner", "What's the capital of Australia?", at(20, 43));
    append(&tx, "voice", "v-1", "eve", "Canberra.", at(20, 44));
    append(&tx, "voice", "v-2", "eve", "Morning.", at(21, 0));
    let before = day_texts(&tx);
    assert_eq!(tx.redact(SAID, true), 2);
    assert_eq!(day_texts(&tx), before); // the dry run wrote nothing
    assert_eq!(tx.redact(SAID, false), 2);
    assert_eq!(day_texts(&tx), [FORGOTTEN, FORGOTTEN, "What's the capital of Australia?", "Canberra.", "Morning."]);
}

#[test]
fn test_eves_own_line_that_matches_does_not_take_the_next_one() {
    let d = tmp();
    let (_, tx, _) = build(d.path());
    append(&tx, "voice", "v-1", "eve", "Your zebra-sentinel code is set.", at(20, 0));
    append(&tx, "voice", "v-1", "eve", "Anything else?", at(20, 1));
    assert_eq!(tx.redact("zebra-sentinel", false), 1);
    assert_eq!(day_texts(&tx), [FORGOTTEN, "Anything else?"]);
}

#[test]
fn test_forget_apply_removes_a_record_fact_in_other_words() {
    let d = tmp();
    let (m, tx, cfg) = build(d.path());
    append(&tx, "voice", "v-1", "owner", SAID, at(20, 41));
    std::fs::create_dir_all(cfg.records_dir()).unwrap();
    let profile = cfg.records_dir().join("profile.md");
    std::fs::write(
        &profile,
        "---\nid: profile\n---\n- Avoid driving for now due to failing the driving test in July (active 2026-08-14).\n- Likes tea.\n",
    )
    .unwrap();
    assert!(read(&profile).contains("failing the driving test")); // the positive control
    let prev = m.forget_preview(SAID);
    assert!(prev["layers"]["records"] == json!(1) && prev["layers"]["transcripts"] == json!(1));
    let done = m.forget_apply(prev["token"].as_str().unwrap());
    assert!(done["ok"] == json!(true) && done["layers"]["records"] == json!(1));
    let left = read(&profile);
    assert!(!left.contains("driving test") && left.contains("Likes tea."));
}

#[test]
fn test_reworded_reaches_the_archive_too() {
    // the dream's stores are records, the index and the archive: the archive's notes lose the reworded line
    let d = tmp();
    let (m, tx, cfg) = build(d.path());
    append(&tx, "voice", "v-1", "owner", SAID, at(20, 41));
    let note = cfg.archive_dir().join("2026-08-14-driving.md");
    std::fs::create_dir_all(cfg.archive_dir()).unwrap();
    std::fs::write(&note, "# Day\n- Failing the driving test in July hurt.\n- Tea.\n").unwrap();
    let prev = m.forget_preview(SAID);
    assert_eq!(prev["layers"]["archive"], json!(1));
    m.forget_apply(prev["token"].as_str().unwrap());
    assert_eq!(read(&note), "# Day\n- Tea.\n");
}
