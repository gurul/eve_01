//! recall.py's tests (bridge/tests/test_recall.py): where memory lives, and the one clause a conversation opens
//! with. The refusals matter most: memory off, nothing said yet, a clock gone backwards, a journal from another day,
//! a forgotten title.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Duration, Local, NaiveDate, TimeZone};
use eve_transcripts::{Append, Dt, TranscriptConfig, Transcripts};

use crate::recall::*;
use crate::testutil::tmp;

fn now() -> Dt {
    let naive = NaiveDate::from_ymd_opt(2026, 9, 11).unwrap().and_hms_opt(21, 30, 0).unwrap();
    Local.from_local_datetime(&naive).single().unwrap().fixed_offset()
}

fn cfg(tmp_path: &Path) -> RecallConfig {
    RecallConfig::new(tmp_path.join("memory"), tmp_path.join("notes"))
}

fn store(cfg: &RecallConfig, enabled: bool) -> Transcripts {
    let n = now();
    Transcripts::new(TranscriptConfig::new(enabled, cfg.transcripts_dir()), None, Some(Arc::new(move || n)))
}

fn said(t: &Transcripts, when: Dt, ch: &str) {
    let conv = format!("{}-1726000000000-abcd", &ch[..1]);
    assert!(t.append_with(ch, &conv, "owner", "say", "hello there", Append { now: Some(when), ..Append::default() }));
}

fn journal(cfg: &RecallConfig, day: &str, title: &str) -> PathBuf {
    let path = cfg.records_dir().join("days").join(format!("{day}.md"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, format!("---\nday: {day}\ntitle: {title}\n---\n\n# {title}\n")).unwrap();
    path
}

fn brief(cfg: &RecallConfig, t: &Transcripts) -> String {
    opening_brief(cfg, Some(t), Some(now()))
}

// ---- nothing to say --------------------------------------------------------------------

#[test]
fn test_memory_off_says_nothing() {
    let d = tmp();
    let c = cfg(d.path());
    let on = store(&c, true);
    said(&on, now() - Duration::days(1), "voice");
    assert_eq!(brief(&c, &on), "You last talked yesterday."); // the control
    assert_eq!(opening_brief(&c, None, Some(now())), "");
    assert_eq!(brief(&c, &store(&c, false)), "");
}

#[test]
fn test_an_empty_or_missing_store_says_nothing() {
    let d = tmp();
    let c = cfg(d.path());
    assert_eq!(brief(&c, &store(&c, true)), "");
    let nope = RecallConfig::new(d.path().join("nope").join("memory"), d.path().join("nope").join("notes"));
    assert_eq!(brief(&nope, &store(&nope, true)), "");
}

// ---- the gap -----------------------------------------------------------------------------

#[test]
fn test_the_gap_is_named_the_way_a_person_would() {
    let d = tmp();
    for (i, (delta, expected)) in [
        (Duration::hours(4), "earlier today"),
        (Duration::days(1), "yesterday"),
        (Duration::days(3), "3 days ago"),
        (Duration::days(9), "last week"),
        (Duration::days(40), "a while ago"),
    ]
    .into_iter()
    .enumerate()
    {
        let c = RecallConfig::new(d.path().join(i.to_string()), d.path().join("notes"));
        let t = store(&c, true);
        said(&t, now() - delta, "voice");
        assert_eq!(brief(&c, &t), format!("You last talked {expected}."), "{expected}");
    }
}

#[test]
fn test_a_conversation_minutes_ago_is_not_worth_mentioning() {
    let d = tmp();
    let c = cfg(d.path());
    let t = store(&c, true);
    said(&t, now() - Duration::minutes(2), "voice");
    assert_eq!(brief(&c, &t), "");
}

#[test]
fn test_a_future_last_turn_is_silence_not_an_error() {
    let d = tmp();
    let c = cfg(d.path());
    let t = store(&c, true);
    said(&t, now() + Duration::days(2), "voice");
    assert_eq!(brief(&c, &t), "");
}

#[test]
fn test_either_channel_counts_but_a_bus_line_does_not() {
    let d = tmp();
    let c = cfg(d.path());
    let t = store(&c, true);
    said(&t, now() - Duration::days(3), "telegram");
    assert_eq!(brief(&c, &t), "You last talked 3 days ago.");
    let opts = Append { tool: "remember".into(), now: Some(now() - Duration::hours(5)), ..Append::default() };
    assert!(t.append_with("bus", "b-1726000000000-abcd", "system", "tool", "a line from the bus", opts));
    let fresh = store(&c, true); // a fresh reader scans the files, not the cache
    assert_eq!(brief(&c, &fresh), "You last talked 3 days ago.");
}

#[test]
fn test_a_broken_store_is_silence() {
    struct Broken;
    impl BriefSource for Broken {
        fn enabled(&self) -> bool {
            true
        }
        fn last_turn_at(&self) -> anyhow::Result<Option<Dt>> {
            Err(std::io::Error::other("disk gone").into())
        }
        fn day_of(&self, _when: Dt) -> anyhow::Result<String> {
            Ok(String::new())
        }
    }
    let d = tmp();
    assert_eq!(opening_brief(&cfg(d.path()), Some(&Broken), Some(now())), "");
}

// ---- the journal title ------------------------------------------------------------------

#[test]
fn test_the_journal_title_of_the_last_day_rides_along() {
    let d = tmp();
    let c = cfg(d.path());
    let t = store(&c, true);
    said(&t, now() - Duration::days(1), "voice");
    journal(&c, "2026-09-10", "Planning the weekend trip");
    assert_eq!(brief(&c, &t), "You last talked yesterday, about: Planning the weekend trip.");
}

#[test]
fn test_a_journal_from_another_day_is_not_quoted() {
    let d = tmp();
    let c = cfg(d.path());
    let t = store(&c, true);
    said(&t, now() - Duration::days(1), "voice");
    journal(&c, "2026-09-08", "Something older");
    assert_eq!(brief(&c, &t), "You last talked yesterday.");
}

#[test]
fn test_a_talk_earlier_today_never_carries_a_title() {
    let d = tmp();
    let c = cfg(d.path());
    let t = store(&c, true);
    said(&t, now() - Duration::hours(4), "voice");
    journal(&c, "2026-09-11", "Today so far");
    assert_eq!(brief(&c, &t), "You last talked earlier today.");
}

#[test]
fn test_a_forgotten_bare_or_questioning_title_is_left_out() {
    for title in ["(forgotten)", "2026-09-10", "Did we fix the servo?"] {
        let d = tmp();
        let c = cfg(d.path());
        let t = store(&c, true);
        said(&t, now() - Duration::days(1), "voice");
        journal(&c, "2026-09-10", title);
        assert_eq!(brief(&c, &t), "You last talked yesterday.", "{title}");
    }
}

#[test]
fn test_a_long_brief_is_cut_to_length_on_a_word() {
    let d = tmp();
    let c = cfg(d.path());
    let t = store(&c, true);
    said(&t, now() - Duration::days(1), "voice");
    journal(&c, "2026-09-10", &"a very long day about the servos ".repeat(12));
    let b = brief(&c, &t);
    assert!(b.chars().count() <= MAX_BRIEF_CHARS);
    assert!(b.starts_with("You last talked yesterday, about: a very long day"));
    assert!(b.ends_with('.') && !b.ends_with(" ."));
}

#[test]
fn test_the_stamp_file_is_gone() {
    // The last-conversation stamp is retired: the transcripts know when the owner last talked.
    let src = include_str!("recall.rs");
    assert!(src.contains("pub fn opening_brief")); // positive control
    for name in ["note_conversation_time", "STAMP_FILE", "newest_eve_note", "newest_daily", "stamp_path", "sessions_dir"] {
        assert!(!src.contains(name), "{name}");
    }
}

// ---- where it lives ----------------------------------------------------------------------

#[test]
fn test_configured_reads_the_memory_dir_and_expands_the_paths() {
    let env: HashMap<String, String> =
        [("EVE_MEMORY_DIR".to_string(), "~/x/memory".to_string()), ("EVE_NOTES_DIR".to_string(), "~/x/notes".to_string())].into();
    let c = configured(Some(&env));
    assert!(c.store.is_absolute() && c.store.file_name().unwrap() == "memory" && !c.store.to_string_lossy().contains('~'));
    assert_eq!(c.transcripts_dir(), c.store.join("transcripts"));
    assert_eq!(c.records_dir(), c.store.join("records"));
    assert_eq!(c.mem0_dir(), c.store.join("mem0"));
    assert_eq!(c.archive_dir(), c.store.join("archive"));
    let default = configured(Some(&HashMap::new()));
    assert_eq!(default.store, eve_transcripts::py::expand_user(Path::new("~/.config/eve-bridge/memory")));
    assert_eq!(default.notes.file_name().unwrap(), "notes");
    // the retired store's variable no longer moves the memory root: it only names the migration's source
    let debrief: HashMap<String, String> = [("EVE_DEBRIEF_DIR".to_string(), "~/x/debrief".to_string())].into();
    assert_eq!(configured(Some(&debrief)).store, default.store);
    let blank: HashMap<String, String> = [("EVE_MEMORY_DIR".to_string(), "   ".to_string())].into();
    assert_eq!(configured(Some(&blank)).store, default.store);
}

#[test]
fn test_the_legacy_locations_are_the_migrations_source() {
    let expand = |p: &str| eve_transcripts::py::expand_user(Path::new(p));
    assert_eq!(legacy_store(Some(&HashMap::new())), expand("~/.config/eve-bridge/debrief"));
    let env: HashMap<String, String> = [("EVE_DEBRIEF_DIR".to_string(), "~/x/debrief".to_string())].into();
    assert_eq!(legacy_store(Some(&env)), expand("~/x/debrief"));
    assert_eq!(legacy_mem0(), expand("~/.config/eve-bridge/mem0"));
}

#[test]
fn test_the_gap_clause_reads_the_calendar_not_the_hours() {
    let at = |d: u32, h: u32| Local.from_local_datetime(&NaiveDate::from_ymd_opt(2026, 9, d).unwrap().and_hms_opt(h, 0, 0).unwrap()).single().unwrap();
    assert_eq!(gap_clause(at(10, 23), at(11, 1)), "yesterday"); // two hours, but another day
    assert_eq!(gap_clause(at(11, 1), at(11, 23)), "earlier today");
    assert_eq!(gap_clause(at(11, 23), at(11, 1)), ""); // the clock went backwards
    assert_eq!(gap_clause(at(4, 12), at(11, 12)), "last week");
    assert_eq!(gap_clause(at(5, 12), at(11, 12)), "6 days ago");
}
