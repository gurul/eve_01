//! Port of `bridge/tests/test_notes_widget.py` (the pure half), plus the parts of `test_photos.py` and
//! `test_diary.py` that exercise this module.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::NaiveDate;
use eve_records::RecallConfig;

use crate as nw;
use crate::Note;

fn d(y: i32, m: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, day).unwrap()
}

const TODAY: (i32, u32, u32) = (2026, 9, 5);
const YESTERDAY: (i32, u32, u32) = (2026, 9, 4);

fn today() -> NaiveDate {
    d(TODAY.0, TODAY.1, TODAY.2)
}

fn yesterday() -> NaiveDate {
    d(YESTERDAY.0, YESTERDAY.1, YESTERDAY.2)
}

fn write(directory: &Path, day: NaiveDate, lines: &[String]) -> PathBuf {
    let path = directory.join(format!("{}.md", day.format("%Y-%m-%d")));
    fs::write(&path, lines.join("\n") + "\n").unwrap();
    path
}

fn s(lines: &[&str]) -> Vec<String> {
    lines.iter().map(|l| l.to_string()).collect()
}

fn names(paths: &[PathBuf]) -> Vec<String> {
    paths.iter().map(|p| p.file_name().unwrap().to_string_lossy().into_owned()).collect()
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// --- discovery -------------------------------------------------------------

#[test]
fn test_note_files_only_today_and_yesterday_newest_first() {
    let t = tmp();
    for day in [d(2026, 9, 1), yesterday(), today()] {
        write(t.path(), day, &s(&["- 09:00 yaw=+0 pitch=0 — old"]));
    }
    fs::write(t.path().join("notes.txt"), "- 09:00 not a note file").unwrap();
    let files = nw::note_files(t.path(), Some(today()));
    assert_eq!(names(&files), ["2026-09-05.md", "2026-09-04.md"]);
}

#[test]
fn test_note_files_skips_missing_days() {
    let t = tmp();
    write(t.path(), yesterday(), &s(&["- 09:00 yaw=+0 pitch=0 — only yesterday"]));
    assert_eq!(names(&nw::note_files(t.path(), Some(today()))), ["2026-09-04.md"]);
    assert!(nw::note_files(&t.path().join("missing"), Some(today())).is_empty());
}

#[test]
fn test_notes_dir_env_override() {
    let t = tmp();
    let mut env = HashMap::new();
    assert_eq!(nw::notes_dir_in(&env), nw::default_notes_dir());
    env.insert(nw::NOTES_DIR_ENV.to_string(), t.path().to_string_lossy().into_owned());
    assert_eq!(nw::notes_dir_in(&env), t.path());
    // an empty value is unset, as `if raw` reads it
    env.insert(nw::NOTES_DIR_ENV.to_string(), String::new());
    assert_eq!(nw::notes_dir_in(&env), nw::default_notes_dir());
    assert!(nw::default_notes_dir().ends_with(".config/eve-bridge/notes"));
}

#[test]
fn memory_dir_env_override() {
    let mut env = HashMap::new();
    assert_eq!(nw::memory_dir_in(&env), nw::default_memory_dir());
    env.insert(nw::MEMORY_DIR_ENV.to_string(), "/tmp/eve-mem/".to_string());
    assert_eq!(nw::memory_dir_in(&env), PathBuf::from("/tmp/eve-mem"));
    assert!(nw::position_path().ends_with(".config/eve-bridge/widget.json"));
}

#[test]
fn test_day_of_parses_only_dated_filenames() {
    assert_eq!(nw::day_of(Path::new("2026-09-05.md")), Some(today()));
    assert_eq!(nw::day_of(Path::new("2026-13-05.md")), None);
    assert_eq!(nw::day_of(Path::new("README.md")), None);
    // Python's date has no year 0
    assert_eq!(nw::day_of(Path::new("0000-01-01.md")), None);
}

// --- parsing ---------------------------------------------------------------

#[test]
fn test_parse_notes_keeps_note_lines_in_file_order() {
    let text = "# 2026-09-05\n\
                \n\
                - 10:02 yaw=+20 pitch=40 — a mug, still steaming\n\
                not a note\n\
                - 10:15 yaw=-10 pitch=30 — the window is open  \n";
    let notes = nw::parse_notes(text, today());
    assert_eq!(
        notes,
        vec![
            Note::new(today(), "10:02", "yaw=+20 pitch=40 — a mug, still steaming"),
            Note::new(today(), "10:15", "yaw=-10 pitch=30 — the window is open"),
        ]
    );
}

#[test]
fn test_collect_notes_newest_first_across_days() {
    let t = tmp();
    write(t.path(), yesterday(), &s(&["- 22:10 yaw=+0 pitch=0 — y1", "- 23:40 yaw=+0 pitch=0 — y2"]));
    write(t.path(), today(), &s(&["- 08:00 yaw=+0 pitch=0 — t1", "- 09:30 yaw=+0 pitch=0 — t2"]));
    let got: Vec<(NaiveDate, String)> =
        nw::collect_notes(t.path(), Some(today()), nw::MAX_NOTES).into_iter().map(|n| (n.day, n.time)).collect();
    assert_eq!(
        got,
        vec![
            (today(), "09:30".to_string()),
            (today(), "08:00".to_string()),
            (yesterday(), "23:40".to_string()),
            (yesterday(), "22:10".to_string()),
        ]
    );
}

#[test]
fn test_collect_notes_applies_limit_after_ordering() {
    let t = tmp();
    let y: Vec<String> = (0..5).map(|i| format!("- 0{i}:00 yaw=+0 pitch=0 — y{i}")).collect();
    let tt: Vec<String> = (0..5).map(|i| format!("- 1{i}:00 yaw=+0 pitch=0 — t{i}")).collect();
    write(t.path(), yesterday(), &y);
    write(t.path(), today(), &tt);
    let got = nw::collect_notes(t.path(), Some(today()), 3);
    assert_eq!(
        got.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(),
        ["yaw=+0 pitch=0 — t4", "yaw=+0 pitch=0 — t3", "yaw=+0 pitch=0 — t2"]
    );
    assert_eq!(nw::collect_notes(t.path(), Some(today()), nw::MAX_NOTES).len(), 10);
}

#[test]
fn test_collect_notes_missing_dir_is_empty() {
    let t = tmp();
    assert!(nw::collect_notes(&t.path().join("nope"), Some(today()), nw::MAX_NOTES).is_empty());
}

// --- rendering -------------------------------------------------------------

#[test]
fn test_render_text_empty_state() {
    assert_eq!(nw::render_text(&[], Some(today()), &[]), nw::EMPTY_TEXT);
    // the empty state teaches the two things you can say, because the widget is the interface and the voice is
    // the way in
    assert!(nw::EMPTY_TEXT.contains("hey eve") && nw::EMPTY_TEXT.contains("remember that"));
}

/// What eve heard and what eve saw are different kinds of claim, so a day holding both gets a labelled group for
/// each.
#[test]
fn test_the_two_provenances_are_never_mixed_into_one_list() {
    let said = Note::with_kind(today(), "22:14", "The right servo", nw::SAID);
    let owes = Note::with_kind(today(), "22:14", "eve owes an answer", nw::SAID);
    let seen = Note::with_kind(today(), "21:03", "the chair is pushed in", nw::SEEN);

    let sections = nw::render_sections(&[said, owes, seen.clone()], Some(today()));
    let labels: Vec<&str> = sections.iter().map(|(l, _)| l.as_str()).collect();
    assert!(labels.iter().any(|l| l.contains("Talking")));
    assert!(labels.iter().any(|l| l.contains("Looking")));
    assert_eq!(labels, ["Today · 2026-09-05 · Talking", "Looking"]);
    assert_eq!(sections[0].1.len(), 2);
    assert_eq!(sections[1].1, vec![seen.clone()]);

    // a day with only one kind needs no sub-heading
    let only_seen: Vec<String> =
        nw::render_sections(&[seen], Some(today())).into_iter().map(|(l, _)| l).collect();
    assert_eq!(only_seen, [nw::day_label(today(), Some(today()))]);
}

#[test]
fn test_starred_claims_are_pinned_above_the_days() {
    let seen = Note::with_kind(today(), "21:03", "the chair is pushed in", nw::SEEN);
    let text = nw::render_text(&[seen], Some(today()), &["The owner keeps the guitar behind the desk".to_string()]);
    assert!(text.find(nw::STARS_LABEL).unwrap() < text.find("the chair is pushed in").unwrap());
    assert!(text.contains("guitar behind the desk"));
}

#[test]
fn starred_claims_render_newest_first_with_a_star() {
    let text = nw::render_text(&[], Some(today()), &["Older claim".to_string(), "Newer claim".to_string()]);
    assert_eq!(text, "Remembered for good\n\u{2605}  Newer claim\n\u{2605}  Older claim");
}

const JOURNAL: &str = "---
day: 2026-09-05
title: The right servo and the flash script
---

# The right servo and the flash script

## What happened

- the owner thinks the right servo sticks

## Still open

- eve owes an answer about why the right servo sticks
- the owner wants to try a different horn
";

#[test]
fn test_a_day_journal_shows_its_title_and_what_eve_owes() {
    assert_eq!(
        nw::parse_conversation(JOURNAL),
        ["The right servo and the flash script", "eve owes an answer about why the right servo sticks"],
        "the title and eve's own debt; the owner's threads are not the widget's job"
    );

    let t = tmp();
    let days = t.path().join("records").join("days");
    fs::create_dir_all(&days).unwrap();
    fs::write(days.join("2026-09-05.md"), JOURNAL).unwrap();
    fs::write(days.join("2026-01-01.md"), "# An old day\n").unwrap(); // outside the two days: not shown
    let got = nw::collect_conversations(t.path(), Some(today()), nw::MAX_NOTES);
    assert_eq!(
        got.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(),
        ["The right servo and the flash script", "eve owes an answer about why the right servo sticks"]
    );
    assert_eq!(got.iter().map(|n| n.time.as_str()).collect::<Vec<_>>(), [nw::NO_TIME, nw::NO_TIME]);
    assert!(got.iter().all(|n| n.kind == nw::SAID));
}

#[test]
fn test_a_forgotten_journal_line_is_never_shown() {
    assert_eq!(
        nw::parse_conversation("# (forgotten)\n\n- eve owes (forgotten)\n- eve owes a photo\n"),
        ["eve owes a photo"]
    );
}

#[test]
fn only_the_first_two_debts_are_shown_and_case_does_not_matter() {
    let text = "# A day\n- Eve Owes one\n- eve owes two\n- EVE OWES three\n";
    assert_eq!(nw::parse_conversation(text), ["A day", "Eve Owes one", "eve owes two"]);
}

#[test]
fn collect_conversations_stops_at_the_limit() {
    let t = tmp();
    let days = t.path().join("records").join("days");
    fs::create_dir_all(&days).unwrap();
    fs::write(days.join("2026-09-05.md"), JOURNAL).unwrap();
    let got = nw::collect_conversations(t.path(), Some(today()), 1);
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].text, "The right servo and the flash script");
}

#[test]
fn test_the_stars_come_from_the_records_starred_page() {
    let t = tmp();
    assert!(nw::stars(t.path(), nw::MAX_STARS).is_empty());
    let cfg = RecallConfig::new(t.path(), t.path().join("notes"));
    assert!(eve_records::star(&cfg, "the spare key is under the blue pot", None).is_some());
    assert_eq!(nw::stars(t.path(), nw::MAX_STARS), ["The spare key is under the blue pot"]);
}

#[test]
fn test_everything_is_gathered_newest_first_and_absence_is_normal() {
    let t = tmp();
    let notes_dir = t.path().join("notes");
    let store = t.path().join("memory");
    fs::create_dir_all(&notes_dir).unwrap();
    fs::write(notes_dir.join("2026-09-05.md"), "- 21:03 the chair is pushed in\n").unwrap();
    let days = store.join("records").join("days");
    fs::create_dir_all(&days).unwrap();
    fs::write(days.join("2026-09-04.md"), "---\ntitle: A chat\n---\n# A chat\n").unwrap();

    let (notes, starred) = nw::collect_everything(&notes_dir, &store, Some(today()), nw::MAX_NOTES);
    assert_eq!(notes.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(), ["the chair is pushed in", "A chat"]);
    assert!(starred.is_empty());

    // either half missing is a normal state, not an error
    assert!(!nw::collect_everything(&notes_dir, &t.path().join("nope"), Some(today()), nw::MAX_NOTES).0.is_empty());
    assert!(!nw::collect_everything(&t.path().join("nope"), &store, Some(today()), nw::MAX_NOTES).0.is_empty());
    assert_eq!(
        nw::collect_everything(&t.path().join("a"), &t.path().join("b"), Some(today()), nw::MAX_NOTES),
        (vec![], vec![])
    );
}

#[test]
fn a_journal_line_sorts_below_a_timed_note_of_the_same_day() {
    let t = tmp();
    let notes_dir = t.path().join("notes");
    let store = t.path().join("memory");
    fs::create_dir_all(&notes_dir).unwrap();
    fs::write(notes_dir.join("2026-09-05.md"), "- 07:00 early\n").unwrap();
    let days = store.join("records").join("days");
    fs::create_dir_all(&days).unwrap();
    fs::write(days.join("2026-09-05.md"), "# Talked\n").unwrap();
    let (notes, _) = nw::collect_everything(&notes_dir, &store, Some(today()), nw::MAX_NOTES);
    assert_eq!(notes.iter().map(|n| n.text.as_str()).collect::<Vec<_>>(), ["early", "Talked"]);
}

#[test]
fn test_render_text_sections_per_day_with_headers() {
    let notes = vec![
        Note::new(today(), "09:30", "yaw=+5 pitch=10 — t2"),
        Note::new(today(), "08:00", "yaw=+5 pitch=10 — t1"),
        Note::new(yesterday(), "23:40", "yaw=+5 pitch=10 — y1"),
    ];
    assert_eq!(
        nw::render_text(&notes, Some(today()), &[]).lines().collect::<Vec<_>>(),
        [
            "Today · 2026-09-05",
            "09:30  yaw=+5 pitch=10 — t2",
            "08:00  yaw=+5 pitch=10 — t1",
            "",
            "Yesterday · 2026-09-04",
            "23:40  yaw=+5 pitch=10 — y1",
        ]
    );
}

#[test]
fn test_day_label_falls_back_to_iso_for_other_days() {
    assert_eq!(nw::day_label(d(2026, 9, 1), Some(today())), "2026-09-01");
}

// --- position persistence --------------------------------------------------

#[test]
fn test_position_round_trip() {
    let t = tmp();
    let path = t.path().join("cfg").join("widget.json");
    assert_eq!(nw::load_position(&path), None);
    nw::save_position(1234.5, 88.0, &path).unwrap();
    assert_eq!(nw::load_position(&path), Some((1234.5, 88.0)));
    assert!(!t.path().join("cfg").join("widget.json.tmp").exists());
    // the bytes json.dumps({"x": round(x, 1), "y": round(y, 1)}) writes
    assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"x": 1234.5, "y": 88.0}"#);
    nw::save_position(0.25, -3.0, &path).unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), r#"{"x": 0.2, "y": -3.0}"#);
}

#[test]
fn test_load_position_tolerates_garbage() {
    for payload in ["", "{}", "[1,2]", r#"{"x": "far", "y": 1}"#, "not json"] {
        let t = tmp();
        let path = t.path().join("widget.json");
        fs::write(&path, payload).unwrap();
        assert_eq!(nw::load_position(&path), None, "{payload:?}");
    }
}

#[test]
fn load_position_reads_what_float_accepts() {
    let t = tmp();
    let path = t.path().join("widget.json");
    fs::write(&path, r#"{"x": " 12.5 ", "y": true, "z": 1}"#).unwrap();
    assert_eq!(nw::load_position(&path), Some((12.5, 1.0)));
    fs::write(&path, r#"{"x": "1_000", "y": 7}"#).unwrap();
    assert_eq!(nw::load_position(&path), Some((1000.0, 7.0)));
    fs::write(&path, r#"{"x": "1__0", "y": 7}"#).unwrap();
    assert_eq!(nw::load_position(&path), None);
    fs::write(&path, r#"{"x": null, "y": 7}"#).unwrap();
    assert_eq!(nw::load_position(&path), None);
}

#[test]
fn the_learning_port_reads_as_python_int_does() {
    assert_eq!(nw::py_int_str("48766"), Some(48766));
    assert_eq!(nw::py_int_str(" +48_766\n"), Some(48766));
    assert_eq!(nw::py_int_str("-1"), Some(-1));
    assert_eq!(nw::py_int_str("\u{0664}\u{0668}"), Some(48));
    for bad in ["", "  ", "+", "4__8", "_48", "48_", "4.8", "0x10", "--1"] {
        assert_eq!(nw::py_int_str(bad), None, "{bad:?}");
    }
}

#[test]
fn test_default_position_is_top_right_with_margin() {
    let screen = (0.0, 0.0, 1920.0, 1055.0); // visible frame: menu bar already removed
    assert_eq!(nw::default_position(screen, (360.0, 420.0), 24.0), (1536.0, 611.0));
}

#[test]
fn test_position_on_screens_rejects_offscreen_saved_origin() {
    let screens = [(0.0, 0.0, 1920.0, 1080.0)];
    assert!(nw::position_on_screens(Some((1536.0, 611.0)), &screens, nw::WIDGET_SIZE));
    assert!(!nw::position_on_screens(Some((2500.0, 611.0)), &screens, nw::WIDGET_SIZE)); // unplugged display
    assert!(!nw::position_on_screens(None, &screens, nw::WIDGET_SIZE));
}

// --- the watcher's filter --------------------------------------------------

#[test]
fn the_watcher_ignores_git_and_editor_files() {
    assert!(nw::change_matters(Path::new("/u/.config/eve-bridge/notes/2026-09-05.md")));
    assert!(nw::change_matters(Path::new("/u/memory/records/starred.md")));
    assert!(!nw::change_matters(Path::new("/u/memory/records/.git/index")));
    assert!(!nw::change_matters(Path::new("/u/notes/.2026-09-05.md.swp")));
    assert!(!nw::change_matters(Path::new("/u/notes/2026-09-05.md~")));
    assert!(!nw::change_matters(Path::new("/u/notes/.DS_Store")));
}

// --- tests_ref: test_photos.py / test_diary.py -----------------------------

#[test]
fn test_photo_line_is_an_indented_image_that_is_not_a_note() {
    let line = "  ![](photos/2026-09-06/141207-42.jpg)";
    // Both readers key on a leading "- HH:MM"; this line has neither.
    assert!(nw::LINE_RE.captures(line.trim()).is_none());
}

#[test]
fn a_photo_line_under_a_diary_line_is_not_a_second_note() {
    let text = "- 14:12 yaw=+20 pitch=40 — Someone brought a plant to my desk.\n  ![](photos/2026-09-06/141207-1.jpg)\n";
    assert_eq!(nw::parse_notes(text, d(2026, 9, 6)).len(), 1);
}

#[test]
fn render_text_without_today_uses_the_local_date() {
    // `today=None` means date.today(): a note dated today renders under "Today".
    let now = chrono::Local::now().date_naive();
    let text = nw::render_text(&[Note::new(now, "09:00", "x")], None, &[]);
    assert!(text.starts_with("Today · "));
}

#[cfg(not(target_os = "macos"))]
#[test]
fn run_off_mac_refuses() {
    assert_eq!(nw::run(true, None, None, None).unwrap(), 2);
}
