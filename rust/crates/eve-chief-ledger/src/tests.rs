//! The ledger half of bridge/tests/test_chief_record.py (Gate G1.1 of the chief design, 2026-09-29), plus the
//! byte-level checks against what CPython writes.

use super::*;
use chrono::NaiveDate;
use eve_chief_card::{ParseOptions, LIVE};
use std::os::unix::fs::PermissionsExt;

const SAID: &str = "find me a standing desk under $600 and order it";

fn desk_args(over: Value) -> Value {
    let mut args = json!({
        "kind": "job", "title": "Standing desk", "purpose": "a desk for the office",
        "end_state": "an order confirmation for one desk under $600",
        "done_checks": [{"kind": "read_links", "arg": 3}, {"kind": "cited_pick", "arg": null},
                        {"kind": "ui_seen", "arg": "order confirmed"}],
        "never": ["over $600"],
        "phases": [{"do": "research", "goal": "standing desks under $600 with reviews", "door": null},
                   {"do": "assess", "goal": "pick one", "door": null},
                   {"do": "act", "goal": "order the picked desk", "door": null}],
        "deadline": null, "cue": null, "firm": false, "question": null});
    if let (Some(a), Value::Object(o)) = (args.as_object_mut(), over) {
        for (k, v) in o {
            a.insert(k, v);
        }
    }
    args
}

fn reminder_args() -> Value {
    json!({
        "kind": "commitment", "title": "BOM export", "purpose": "the board order", "end_state": "the BOM is exported",
        "done_checks": [], "never": [], "phases": [], "deadline": "2026-10-02 17:00",
        "cue": {"at": "2026-10-01 10:00", "next_action": "export the BOM"}, "firm": true, "question": null})
}

fn card_with(over: Value) -> Card {
    let opts = ParseOptions { said_ref: "2026-09-29 10:02", ..ParseOptions::default() };
    chief_card::parse(&desk_args(over), SAID, &opts).expect("a desk card")
}

fn card() -> Card {
    card_with(json!({}))
}

fn titled(title: &str) -> Card {
    card_with(json!({"title": title}))
}

fn reminder() -> Card {
    chief_card::parse(&reminder_args(), "remind me", &ParseOptions::default()).expect("a reminder")
}

fn clock(t: f64) -> Clock {
    Arc::new(move || t)
}

const T0: f64 = 1_790_000_000.0;

fn local_ts(y: i32, m: u32, d: u32, h: u32, min: u32) -> f64 {
    let at = NaiveDate::from_ymd_opt(y, m, d).unwrap().and_hms_opt(h, min, 0).unwrap();
    py::naive_timestamp(&at).unwrap()
}

fn mode(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o777
}

fn ledger(dir: &Path) -> Ledger {
    Ledger::new(Some(dir))
}

// ---- the folder ----------------------------------------------------------------------------------

#[test]
fn test_the_ledger_is_in_the_temp_folder_during_tests() {
    // conftest's fixture sets EVE_CHIEF_DIR; here the environment is passed, never set on the process
    let tmp = tempfile::tempdir().unwrap();
    let env: HashMap<String, String> =
        [("EVE_CHIEF_DIR".to_string(), tmp.path().to_string_lossy().into_owned())].into();
    assert_eq!(chief_dir(Some(&env)), tmp.path());
    assert_eq!(Ledger::new(Some(&chief_dir(Some(&env)))).dir(), tmp.path());
    let home = py::expanduser("~");
    assert!(!chief_dir(Some(&env)).to_string_lossy().contains(&*home.join(".config").to_string_lossy()));
}

#[test]
fn test_positive_control_unset_the_ledger_resolves_to_the_config_folder() {
    let home = py::expanduser("~");
    let empty = HashMap::new();
    assert_eq!(chief_dir(Some(&empty)), home.join(".config").join("eve-bridge").join("chief"));
    let blank: HashMap<String, String> = [("EVE_CHIEF_DIR".to_string(), "  ".to_string())].into();
    assert_eq!(chief_dir(Some(&blank)), home.join(".config/eve-bridge/chief"));
    let moved: HashMap<String, String> = [("EVE_CHIEF_DIR".to_string(), "/x/y".to_string())].into();
    assert_eq!(chief_dir(Some(&moved)), PathBuf::from("/x/y"));
    let tilde: HashMap<String, String> = [("EVE_CHIEF_DIR".to_string(), " ~/elsewhere/ ".to_string())].into();
    assert_eq!(chief_dir(Some(&tilde)), home.join("elsewhere"));
}

// ---- the store -----------------------------------------------------------------------------------

#[test]
fn test_new_files_the_card_under_a_fresh_id_and_logs_created() {
    let tmp = tempfile::tempdir().unwrap();
    let led = Ledger::with_clock(Some(tmp.path()), clock(T0));
    let a = led.new_card(card(), "take_on").unwrap();
    let b = led.new_card(titled("Monitor arm"), "take_on").unwrap();
    assert_eq!((a.id.as_str(), b.id.as_str()), ("c1", "c2"));
    assert_eq!(a.created, T0);
    assert_eq!(mode(&led.cards_path()), 0o600);
    assert_eq!(mode(&led.events_path()), 0o600);
    let again = ledger(tmp.path());
    assert_eq!(again.get("c1"), Some(a.clone()));
    assert_eq!(again.get("c2"), Some(b));
    let created = again.events(Some("c1"));
    assert_eq!(created[0]["e"], "created");
    assert_eq!(created[0]["doors"], json!(["two_way", "two_way", "one_way"]));
    assert!(matches!(led.new_card(a, "take_on"), Err(Error::Value(_)))); // already filed
}

#[test]
fn test_ids_are_never_reused() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    for _ in 0..3 {
        led.new_card(card(), "take_on").unwrap();
    }
    assert_eq!(led.redact(["c3"]).unwrap(), 1);
    assert_eq!(ledger(tmp.path()).new_card(card(), "take_on").unwrap().id, "c4"); // c3 is gone, and never comes back
    // the cards file lost entirely: the events still name c1..c4, so the next id is c5
    fs::remove_file(led.cards_path()).unwrap();
    assert_eq!(ledger(tmp.path()).new_card(card(), "take_on").unwrap().id, "c5");
}

fn boom(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::from_raw_os_error(libc::ENOSPC))
}

#[test]
fn test_a_failed_replace_leaves_the_old_file_and_memory_as_they_were() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    led.new_card(card(), "take_on").unwrap();
    let before = fs::read(led.cards_path()).unwrap();
    led.set_replace(boom);
    assert!(matches!(led.new_card(titled("Second"), "take_on"), Err(Error::Ledger(_))));
    assert_eq!(
        led.update("c1", "test", StateUpdate::new().status("dropped")),
        Err(Error::Ledger(LedgerError("could not save the cards (OSError)".into())))
    );
    assert_eq!(fs::read(led.cards_path()).unwrap(), before);
    assert!(led.get("c2").is_none());
    assert_eq!(led.get("c1").unwrap().status, "active");
    let temps: Vec<_> = fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with(".cards-"))
        .collect();
    assert!(temps.is_empty()); // no temp file left
    led.set_replace(os_replace);
    assert_eq!(led.new_card(titled("Second"), "take_on").unwrap().id, "c2");
}

#[test]
fn test_an_unreadable_file_moves_to_bad_and_is_reported_once() {
    let tmp = tempfile::tempdir().unwrap();
    let led = Ledger::with_clock(Some(tmp.path()), clock(T0));
    led.new_card(card(), "take_on").unwrap();
    led.new_card(card(), "take_on").unwrap();
    fs::write(led.cards_path(), r#"{"version": 1, "last_id": 2, "cards": [{"id": "c2", oops"#).unwrap();
    fs::remove_file(led.events_path()).unwrap();
    let later = 1_790_000_500.0;
    let again = Ledger::with_clock(Some(tmp.path()), clock(later));
    let aside = tmp.path().join("cards.json.bad-1790000500");
    assert!(aside.exists() && !again.cards_path().exists());
    assert!(again.all().is_empty());
    let problem = again.take_problem();
    assert!(problem.contains("cards.json.bad-1790000500"), "{problem}");
    assert_eq!(
        problem,
        format!(
            "I couldn't read my list of jobs, so I'm starting with none. The old file is kept as \
             cards.json.bad-1790000500 in {}.",
            tmp.path().display()
        )
    );
    assert_eq!(again.take_problem(), "");
    assert_eq!(again.new_card(card(), "take_on").unwrap().id, "c3"); // ids go on past the bad file's highest
}

#[test]
fn test_one_card_that_does_not_load_is_skipped_not_the_whole_list() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    led.new_card(card(), "take_on").unwrap();
    led.new_card(card(), "take_on").unwrap();
    let mut raw: Value = serde_json::from_slice(&fs::read(led.cards_path()).unwrap()).unwrap();
    raw["cards"][0]["status"] = json!("finished");
    fs::write(led.cards_path(), serde_json::to_string(&raw).unwrap()).unwrap();
    let again = ledger(tmp.path());
    assert_eq!(again.all().iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["c2"]);
    assert!(again.take_problem().contains("(c1)"));
    assert_eq!(again.take_problem(), ""); // said once (it was "" before 2026-09-29)
}

#[test]
fn test_state_changes_do_not_bump_rev_and_a_revision_does() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    let c = led.new_card(card(), "take_on").unwrap();
    led.update_phase(&c.id, 3, "go asked", PhaseUpdate::new().approval(Some(Approval::new("tok", 1, 1.0)))).unwrap();
    let s = led.update(&c.id, "go asked", StateUpdate::new().status("waiting").waiting_for(Some("go"))).unwrap();
    assert_eq!(s.rev, 1);
    assert_eq!(s.phase(3).unwrap().approval, Some(Approval::new("tok", 1, 1.0)));
    let patch = json!({"never": ["over $500"]});
    let r = led
        .revise(&c.id, patch.as_object().unwrap(), "owner changed the cap", "change c1 never over $500", &LIVE, None)
        .unwrap()
        .unwrap();
    assert_eq!(r.rev, 2);
    assert!(r.phase(3).unwrap().approval.is_none());
    let kinds: Vec<String> = led.events(Some(&c.id)).iter().map(|e| e["e"].as_str().unwrap().to_string()).collect();
    assert_eq!(kinds, ["created", "state", "state", "revised"]);
    // "not a state field" (title, a phase's door) cannot be written: StateUpdate and PhaseUpdate hold only the
    // state fields
    assert_eq!(
        led.update(&c.id, "x", StateUpdate::new().status("finished")),
        Err(Error::Value("unknown status 'finished'".into()))
    );
    assert_eq!(
        led.update(&c.id, "x", StateUpdate::new().waiting_for(Some("tea"))),
        Err(Error::Value("unknown waiting_for 'tea'".into()))
    );
    assert_eq!(
        led.update_phase(&c.id, 1, "x", PhaseUpdate::new().status("paused")),
        Err(Error::Value("unknown phase status 'paused'".into()))
    );
    assert_eq!(led.update_phase(&c.id, 9, "x", PhaseUpdate::new()), Err(Error::Key("c1 has no phase 9".into())));
    led.update(&c.id, "done", StateUpdate::new().status("done")).unwrap();
    // a finished card is reopened, not moved
    assert_eq!(
        led.update(&c.id, "x", StateUpdate::new().status("active")),
        Err(Error::Value("c1 is done: reopen it instead".into()))
    );
    assert!(matches!(led.update_phase(&c.id, 1, "x", PhaseUpdate::new().status("queued")), Err(Error::Value(_))));
    // the same status (or none) on a finished card is a state change still
    assert!(led.update(&c.id, "x", StateUpdate::new().nudges(Nudges::default())).is_ok());
    assert_eq!(led.reopen(&c.id, "owner reopened").unwrap().status, "active");
    assert_eq!(led.get(&c.id).unwrap().rev, 3);
    let empty = Map::new();
    assert_eq!(
        led.revise("c99", &empty, "x", "x", &LIVE, None).unwrap(),
        Err(Refusal::new("there is no card c99"))
    );
    assert_eq!(led.reopen("c99", "x"), Err(Error::Key("c99".into())));
}

#[test]
fn test_an_event_that_cannot_be_written_says_so() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    assert!(led.event(EventKind::Dispatched, "c1", &[("phase", json!(3)), ("executor", json!("codex"))]));
    fs::remove_file(led.events_path()).unwrap();
    fs::create_dir(led.events_path()).unwrap(); // the path is a folder now: the write fails
    assert!(!led.event(EventKind::Dispatched, "c1", &[("phase", json!(3))]));
    let bad = "teleported".parse::<EventKind>();
    assert_eq!(bad, Err(Error::Value("unknown event kind 'teleported'".into())));
    for k in EVENT_KINDS {
        assert_eq!(k.parse::<EventKind>().unwrap().as_str(), *k);
    }
}

#[test]
fn test_events_carry_scalars_capped() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    led.event(
        EventKind::Result,
        "c1",
        &[("why", json!("x".repeat(500))), ("nested", json!({"a": 1})), ("doors", json!(["one_way"]))],
    );
    let row = &led.events(Some("c1"))[0];
    assert_eq!(row["why"].as_str().unwrap().chars().count(), MAX_EVENT_TEXT);
    assert_eq!(row["nested"], json!("{'a': 1}"));
    assert_eq!(row["doors"], json!(["one_way"]));
}

#[test]
fn test_due_finds_cues_and_nudges_that_have_come() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    let c = led.new_card(reminder(), "take_on").unwrap();
    let cue = local_ts(2026, 10, 1, 10, 0);
    assert!(led.due(cue - 60.0).is_empty());
    assert_eq!(led.due(cue + 60.0).iter().map(|x| x.id.clone()).collect::<Vec<_>>(), std::slice::from_ref(&c.id));
    let nudged = Nudges { sent: 1, next_at: Some(cue + 3600.0), ..Nudges::default() };
    led.update(&c.id, "nudged", StateUpdate::new().nudges(nudged)).unwrap();
    assert!(led.due(cue + 60.0).is_empty());
    assert_eq!(led.due(cue + 3601.0).iter().map(|x| x.id.clone()).collect::<Vec<_>>(), std::slice::from_ref(&c.id));
    led.update(&c.id, "done", StateUpdate::new().status("done")).unwrap();
    assert!(led.due(cue + 99999.0).is_empty());
}

#[test]
fn test_for_turn_is_one_short_line() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    assert_eq!(led.for_turn(0.0), "");
    let a = led.new_card(card(), "take_on").unwrap();
    led.update(&a.id, "go", StateUpdate::new().status("waiting").waiting_for(Some("go"))).unwrap();
    led.new_card(titled("Monitor arm"), "take_on").unwrap();
    let r = led.new_card(reminder(), "take_on").unwrap();
    let now = local_ts(2026, 10, 1, 9, 0);
    let line = led.for_turn(now);
    assert_eq!(
        line,
        format!(
            "Open jobs: 2 (c1 waiting on your Go: order the picked desk; c2 researching: Monitor arm). \
             Due today: BOM export ({}).",
            r.id
        )
    );
    for i in 0..20 {
        led.new_card(titled(&format!("Job number {i}")), "take_on").unwrap();
    }
    let long = led.for_turn(now);
    assert!(long.chars().count() <= TURN_LINE_CHARS);
    assert!(long.ends_with('…') && long.starts_with("Open jobs: 22 ("));
}

#[test]
fn test_matching_finds_the_card_the_owners_words_are_about() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    let r = led.new_card(reminder(), "take_on").unwrap();
    led.new_card(card(), "take_on").unwrap();
    assert_eq!(
        led.matching("exporting now: the BOM export for the board").iter().map(|c| c.id.clone()).collect::<Vec<_>>(),
        [r.id]
    );
    assert!(led.matching("what's the weather").is_empty());
}

#[test]
fn test_redact_removes_the_cards_and_their_events() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    let a = led.new_card(card(), "take_on").unwrap();
    let b = led.new_card(titled("Monitor arm"), "take_on").unwrap();
    assert_eq!(led.redact([a.id.as_str()]).unwrap(), 1);
    assert!(led.get(&a.id).is_none() && led.get(&b.id).is_some());
    assert!(led.events(None).iter().all(|e| e.get("id") != Some(&json!(a.id))));
    let kinds: Vec<Value> = led.events(Some(&b.id)).iter().map(|e| e["e"].clone()).collect();
    assert_eq!(kinds, [json!("created")]);
    assert_eq!(mode(&led.events_path()), 0o600);
    let last = led.events(None).pop().unwrap();
    assert_eq!((last["e"].clone(), last["id"].clone(), last["count"].clone()), (json!("redacted"), json!(""), json!(1)));
}

// ---- the reviewer's attacks after P3 (2026-09-29): each was reproduced first, then fixed ----------------

#[test]
fn test_attack_a_number_too_big_sets_the_card_aside_instead_of_crashing() {
    let cases: &[(&[&str], &str)] = &[
        (&["budget", "wall_min"], "1e999"),
        (&["spent", "usd"], "1e999"),
        (&["nudges", "sent"], "-1e999"),
        (&["created"], "1e999"),
    ];
    for (field, value) in cases {
        let tmp = tempfile::tempdir().unwrap();
        let led = ledger(tmp.path());
        led.new_card(card(), "take_on").unwrap();
        led.new_card(card(), "take_on").unwrap();
        let mut raw: Value = serde_json::from_slice(&fs::read(led.cards_path()).unwrap()).unwrap();
        let mut target = &mut raw["cards"][0];
        for key in &field[..field.len() - 1] {
            target = &mut target[*key];
        }
        target[field[field.len() - 1]] = json!("SENTINEL");
        fs::write(led.cards_path(), serde_json::to_string(&raw).unwrap().replace("\"SENTINEL\"", value)).unwrap();
        let again = ledger(tmp.path());
        assert_eq!(again.all().iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["c2"], "{field:?}");
        assert!(again.take_problem().contains("c1"));
    }
}

#[test]
fn test_attack_a_card_that_does_not_load_is_kept_aside_and_reported() {
    let tmp = tempfile::tempdir().unwrap();
    let led = Ledger::with_clock(Some(tmp.path()), clock(T0));
    led.new_card(card(), "take_on").unwrap();
    let mut raw: Value = serde_json::from_slice(&fs::read(led.cards_path()).unwrap()).unwrap();
    raw["cards"][0]["status"] = json!("Active"); // a hand-edit's typo
    fs::write(led.cards_path(), serde_json::to_string(&raw).unwrap()).unwrap();
    let again = Ledger::with_clock(Some(tmp.path()), clock(T0));
    let problem = again.take_problem();
    assert!(problem.contains("1 job") && problem.contains("cards.json.bad-"), "{problem}");
    assert_eq!(
        problem,
        format!(
            "1 job in my list didn't load (c1), so I set it aside. The whole file is kept as \
             cards.json.bad-1790000000 in {}.",
            tmp.path().display()
        )
    );
    again.new_card(card(), "take_on").unwrap(); // the next save writes only what loaded ...
    let saved: Value = serde_json::from_slice(&fs::read(again.cards_path()).unwrap()).unwrap();
    let ids: Vec<&str> = saved["cards"].as_array().unwrap().iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["c2"]);
    let asides: Vec<PathBuf> = fs::read_dir(tmp.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| file_name(p).starts_with("cards.json.bad-"))
        .collect();
    assert_eq!(asides.len(), 1); // ... and the skipped card is still on disk
    assert!(fs::read_to_string(&asides[0]).unwrap().contains("\"Active\""));
    assert_eq!(mode(&asides[0]), 0o600);
}

#[test]
fn test_attack_a_file_of_the_wrong_shape_is_moved_aside_not_overwritten() {
    for text in [r#"[{"id": "c1"}]"#, r#""cards""#, r#"{"version": 1, "last_id": 3, "cards": {"c1": 1}}"#, "1e999", "NaN"] {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("cards.json"), text).unwrap();
        let led = Ledger::with_clock(Some(tmp.path()), clock(T0));
        assert!(led.all().is_empty());
        assert!(led.take_problem().contains("couldn't read my list"), "{text}");
        let aside = tmp.path().join("cards.json.bad-1790000000");
        assert_eq!(fs::read_to_string(&aside).unwrap(), text);
        led.new_card(card(), "take_on").unwrap();
        assert_eq!(fs::read_to_string(&aside).unwrap(), text);
    }
}

#[test]
fn a_cards_list_that_is_null_is_the_wrong_shape_and_a_missing_one_is_empty() {
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("cards.json"), r#"{"cards": null}"#).unwrap();
    assert!(ledger(tmp.path()).take_problem().contains("couldn't read my list"));
    let tmp = tempfile::tempdir().unwrap();
    fs::write(tmp.path().join("cards.json"), r#"{"version": 1, "last_id": 7}"#).unwrap();
    let led = ledger(tmp.path());
    assert_eq!(led.take_problem(), "");
    assert_eq!(led.new_card(card(), "take_on").unwrap().id, "c8");
}

#[test]
fn a_card_that_is_not_an_object_is_named_by_its_place() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    led.new_card(card(), "take_on").unwrap();
    let mut raw: Value = serde_json::from_slice(&fs::read(led.cards_path()).unwrap()).unwrap();
    raw["cards"].as_array_mut().unwrap().insert(0, json!("SENTINEL"));
    raw["cards"].as_array_mut().unwrap().insert(0, json!({"id": "c40", "rev": 0}));
    let text = serde_json::to_string(&raw).unwrap().replace("\"SENTINEL\"", "Infinity");
    fs::write(led.cards_path(), text).unwrap();
    let again = ledger(tmp.path());
    assert!(again.take_problem().starts_with("2 jobs in my list didn't load (c40, #2), so I set them aside."));
    // the bad file's highest id counts
    assert_eq!(again.new_card(card(), "take_on").unwrap().id, "c41");
}

// ---- review D6: events() re-read and parsed the whole append-only file on every call ----------------

#[test]
fn test_events_parse_only_the_lines_appended_since_the_last_read() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    let a = led.new_card(card(), "take_on").unwrap();
    led.event(EventKind::Checked, &a.id, &[("note", json!("one"))]);
    let first = led.events(None);
    let before = led.event_rows.parsed_lines();
    assert_eq!(led.events(None), first);
    assert_eq!(led.event_rows.parsed_lines(), before); // nothing new: nothing parsed
    led.event(EventKind::Checked, &a.id, &[("note", json!("two"))]);
    let now = led.events(None);
    assert_eq!(&now[..now.len() - 1], &first[..]);
    assert_eq!(now.last().unwrap()["note"], "two");
    assert_eq!(led.event_rows.parsed_lines(), before + 1);
    let other = ledger(tmp.path()); // another writer appends: still seen
    other.event(EventKind::Checked, &a.id, &[("note", json!("three"))]);
    let notes = |l: &Ledger| -> Vec<Value> {
        l.events(Some(&a.id)).iter().map(|e| e.get("note").cloned().unwrap_or(Value::Null)).collect()
    };
    assert_eq!(notes(&led)[notes(&led).len() - 3..], [json!("one"), json!("two"), json!("three")]);
    let path = tmp.path().join(EVENTS_FILE);
    let mut f = OpenOptions::new().append(true).open(&path).unwrap(); // a torn tail is skipped, then read once whole
    write!(f, "{{\"t\":1,\"e\":\"checked\",\"id\":\"{}\",\"note\":\"fo", a.id).unwrap();
    assert_eq!(notes(&led).last().unwrap(), "three");
    writeln!(f, "ur\"}}").unwrap();
    assert_eq!(notes(&led).last().unwrap(), "four");
    let b = led.new_card(titled("Monitor arm"), "take_on").unwrap();
    assert_eq!(led.redact([a.id.as_str()]).unwrap(), 1); // a rewrite replaces the file: read afresh
    assert!(led.events(None).iter().all(|e| e.get("id") != Some(&json!(a.id))));
    let kinds: Vec<Value> = led.events(Some(&b.id)).iter().map(|e| e["e"].clone()).collect();
    assert_eq!(kinds, [json!("created")]);
}

#[test]
fn jsonl_rows_skip_foreign_lines_and_read_a_missing_file_as_empty() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("attention.jsonl");
    let rows = JsonlRows::new();
    assert!(rows.rows(&path).is_empty());
    fs::write(&path, b"[1]\n\n{\"a\": NaN}\nnot json\n{\"b\": \"\xff\"}\n   \n").unwrap();
    assert_eq!(rows.rows(&path), vec![json!({"a": null}).as_object().unwrap().clone(),
                                      json!({"b": "\u{fffd}"}).as_object().unwrap().clone()]);
    fs::write(&path, b"{\"c\": 1}\n").unwrap(); // shrank: read afresh
    assert_eq!(rows.rows(&path), vec![json!({"c": 1}).as_object().unwrap().clone()]);
    fs::remove_file(&path).unwrap();
    assert!(rows.rows(&path).is_empty());
}

// ---- what CPython writes -----------------------------------------------------------------------------

#[test]
fn the_cards_file_is_written_as_json_dump_indent_1() {
    let tmp = tempfile::tempdir().unwrap();
    let led = Ledger::with_clock(Some(tmp.path()), clock(T0));
    let commitment = Card {
        title: "Café run".into(),
        ..chief_card::parse(&reminder_args(), "remind me", &ParseOptions::default()).unwrap()
    };
    led.new_card(commitment, "take_on").unwrap();
    let text = fs::read_to_string(led.cards_path()).unwrap();
    // python3: json.dump({"version": 1, "last_id": 1, "cards": [chief_card.to_json(card)]}, f, indent=1)
    let expected = "{\n \"version\": 1,\n \"last_id\": 1,\n \"cards\": [\n  {\n   \"id\": \"c1\",\n   \"rev\": 1,\n   \
\"kind\": \"commitment\",\n   \"title\": \"Caf\\u00e9 run\",\n   \"purpose\": \"the board order\",\n   \
\"end_state\": \"the BOM is exported\",\n   \"done_checks\": [\n    {\n     \"kind\": \"guru_says_done\",\n     \
\"arg\": \"\"\n    }\n   ],\n   \"never\": [],\n   \"phases\": [],\n   \"deadline\": \"2026-10-02 17:00\",\n   \
\"cue\": {\n    \"at\": \"2026-10-01 10:00\",\n    \"next_action\": \"export the BOM\"\n   },\n   \"firm\": true,\n   \
\"question\": \"\",\n   \"budget\": {\n    \"usd\": 0.5,\n    \"wall_min\": 20\n   },\n   \"spent\": {\n    \
\"usd\": 0.0,\n    \"wall_s\": 0.0\n   },\n   \"status\": \"active\",\n   \"waiting_for\": null,\n   \"source\": {\n    \
\"door\": \"telegram\",\n    \"said_ref\": \"\"\n   },\n   \"nudges\": {\n    \"sent\": 0,\n    \"ignored\": 0,\n    \
\"next_at\": null,\n    \"level\": \"push\"\n   },\n   \"created\": 1790000000.0,\n   \"updated\": 1790000000.0\n  }\n ]\n}";
    assert_eq!(text, expected);
}

#[test]
fn an_event_line_is_compact_json_with_ids_first() {
    let tmp = tempfile::tempdir().unwrap();
    let led = Ledger::with_clock(Some(tmp.path()), clock(1_790_000_000.123_456));
    led.event(
        EventKind::Pushed,
        "c3",
        &[
            ("t", json!(5)),
            ("why", json!("  café\n\tnow  ")),
            ("phases", json!(["a", ["b"], {"c": null}, 2.5])),
            ("n", json!(1e16)),
            ("ok", json!(false)),
            ("gone", Value::Null),
        ],
    );
    let text = fs::read_to_string(led.events_path()).unwrap();
    // python3: json.dumps(row, separators=(",", ":"), ensure_ascii=False)
    assert_eq!(
        text,
        "{\"t\":1790000000.123,\"e\":\"pushed\",\"id\":\"c3\",\"why\":\"café now\",\
\"phases\":[\"a\",\"['b']\",\"{'c': None}\",2.5],\"n\":1e+16,\"ok\":false,\"gone\":null}\n"
    );
    let many: Vec<Value> = (0..30).map(|i| json!(i)).collect();
    led.event(EventKind::Critic, "c3", &[("list", Value::Array(many))]);
    assert_eq!(led.events(Some("c3"))[1]["list"].as_array().unwrap().len(), 20);
}

#[test]
fn the_new_dir_is_made_700() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("a").join("chief");
    let led = ledger(&dir);
    assert!(led.take_problem().is_empty());
    led.new_card(card(), "take_on").unwrap();
    assert_eq!(fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700);
}

// ---- claim and replan (chief.py's dispatcher; the reviewer's 2026-09-29 finding) -------------------------

#[test]
fn claim_starts_a_queued_phase_once_and_only_with_its_go() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    let c = led.new_card(card(), "take_on").unwrap();
    // a two-way phase needs no token
    let started = led.claim(&c.id, 1, 1, "", "", "research", &[("why", json!("first"))]).unwrap().unwrap();
    assert_eq!(started.phase(1).unwrap().status, "running");
    assert_eq!(started.phase(1).unwrap().executor, "research");
    assert!(led.claim(&c.id, 1, 1, "", "", "research", &[]).unwrap().is_none()); // not queued any more
    assert!(led.claim(&c.id, 2, 2, "", "", "assess", &[]).unwrap().is_none()); // another rev
    assert!(led.claim(&c.id, 9, 1, "", "", "assess", &[]).unwrap().is_none()); // no such phase
    assert!(led.claim("c9", 1, 1, "", "", "assess", &[]).unwrap().is_none()); // no such card
    // the one-way act: only with the approval for this rev and this digest
    assert!(led.claim(&c.id, 3, 1, "tok", "abc", "codex", &[]).unwrap().is_none());
    let mut go = Approval::new("tok", 1, 1.0);
    go.what = "abc".into();
    led.update_phase(&c.id, 3, "approved", PhaseUpdate::new().approval(Some(go))).unwrap();
    assert!(led.claim(&c.id, 3, 1, "tok", "abd", "codex", &[]).unwrap().is_none()); // the act changed
    assert!(led.claim(&c.id, 3, 1, "other", "abc", "codex", &[]).unwrap().is_none());
    assert!(led.claim(&c.id, 3, 1, "", "abc", "codex", &[]).unwrap().is_none());
    let acted = led.claim(&c.id, 3, 1, "tok", "abc", "codex", &[("digest", json!("abc"))]).unwrap().unwrap();
    assert_eq!(acted.phase(3).unwrap().status, "running");
    assert!(acted.phase(3).unwrap().approval.is_none()); // the yes is spent
    let dispatched: Vec<Map<String, Value>> =
        led.events(Some(&c.id)).into_iter().filter(|e| e["e"] == "dispatched").collect();
    assert_eq!(dispatched.len(), 2);
    assert_eq!(dispatched[1]["phase"], 3);
    assert_eq!(dispatched[1]["token"], "tok");
    assert_eq!(dispatched[1]["digest"], "abc");
    let last = led.events(Some(&c.id)).pop().unwrap();
    assert_eq!((last["e"].clone(), last["why"].clone(), last["phase_status"].clone()),
               (json!("state"), json!("dispatched"), json!("running")));
    // a dropped card starts nothing
    led.update(&c.id, "dropped", StateUpdate::new().status("dropped")).unwrap();
    assert!(led.claim(&c.id, 2, 1, "", "", "assess", &[]).unwrap().is_none());
}

#[test]
fn claim_starts_nothing_when_the_dispatch_line_cannot_be_written() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    let c = led.new_card(card(), "take_on").unwrap();
    fs::remove_file(led.events_path()).unwrap();
    fs::create_dir(led.events_path()).unwrap();
    assert!(led.claim(&c.id, 1, 1, "", "", "research", &[]).unwrap().is_none());
    assert_eq!(led.get(&c.id).unwrap().phase(1).unwrap().status, "queued");
}

#[test]
fn replan_applies_only_to_the_card_it_was_asked_about() {
    let tmp = tempfile::tempdir().unwrap();
    let led = Ledger::with_clock(Some(tmp.path()), clock(T0));
    let c = led.new_card(card(), "take_on").unwrap();
    led.update_phase(&c.id, 3, "go", PhaseUpdate::new().approval(Some(Approval::new("tok", 1, 1.0)))).unwrap();
    let asked = led.get(&c.id).unwrap();
    let phases = asked.phases.clone();
    let new = led.replan(&c.id, &asked, phases.clone(), &[("why", json!("thrash"))]).unwrap().unwrap();
    assert_eq!(new.rev, 2);
    assert!(new.phases.iter().all(|p| p.approval.is_none()));
    let last = led.events(Some(&c.id)).pop().unwrap();
    assert_eq!((last["e"].clone(), last["rev"].clone(), last["why"].clone()), (json!("replanned"), json!(2), json!("thrash")));
    assert!(led.replan(&c.id, &asked, phases.clone(), &[]).unwrap().is_none()); // it changed meanwhile
    led.update(&c.id, "dropped", StateUpdate::new().status("dropped")).unwrap();
    let dropped = led.get(&c.id).unwrap();
    assert!(led.replan(&c.id, &dropped, phases.clone(), &[]).unwrap().is_none());
    assert!(led.replan("c9", &dropped, phases, &[]).unwrap().is_none());
}

#[test]
fn the_store_gives_the_oldest_finished_cards_way_at_the_cap() {
    let mut cards: IndexMap<String, Card> = IndexMap::new();
    for i in 1..=MAX_CARDS + 2 {
        let mut c = card();
        c.id = format!("c{i}");
        c.updated = (1000 - i) as f64;
        if i % 100 == 0 {
            c.status = "done".into();
        }
        cards.insert(c.id.clone(), c);
    }
    let out = Ledger::trim(cards);
    assert_eq!(out.len(), MAX_CARDS);
    assert!(!out.contains_key("c500") && !out.contains_key("c400"));
    assert!(out.contains_key("c100") && out.contains_key("c502"));
}

#[test]
fn describe_says_what_each_open_card_waits_on() {
    let mut c = card();
    c.id = "c7".into();
    assert_eq!(Ledger::describe(&c), "c7 researching: Standing desk");
    c.status = "waiting".into();
    for (w, text) in [
        ("question", "c7 waiting on your answer: Standing desk"),
        ("budget", "c7 waiting on your budget raise: Standing desk"),
        ("mac_slot", "c7 waiting for the Mac: Standing desk"),
        ("go", "c7 waiting on your Go: order the picked desk"),
    ] {
        c.waiting_for = Some(w.into());
        assert_eq!(Ledger::describe(&c), text);
    }
    c.phases[2].status = "ok".into();
    assert_eq!(Ledger::describe(&c), "c7 waiting on your Go: Standing desk");
    c.status = "unverified".into();
    assert_eq!(Ledger::describe(&c), "c7 unverified: Standing desk");
    c.status = "active".into();
    for p in c.phases.iter_mut() {
        p.status = "ok".into();
    }
    assert_eq!(Ledger::describe(&c), "c7 working: Standing desk");
}

#[test]
fn redact_keeps_torn_lines_and_reads_text_as_python_does() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    let a = led.new_card(card(), "take_on").unwrap();
    let mut f = OpenOptions::new().append(true).open(led.events_path()).unwrap();
    f.write_all(b"not json\r\n{\"e\":\"checked\",\"id\":\"c1\",\"x\":NaN}\n").unwrap();
    drop(f);
    assert_eq!(led.redact([a.id.as_str(), "c77"]).unwrap(), 1);
    let text = fs::read_to_string(led.events_path()).unwrap();
    // the CRLF came back as LF (text mode), the torn line stayed, both c1 lines went, and the redaction is logged
    assert!(text.starts_with("not json\n{\"t\":"), "{text:?}");
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[1].contains("\"e\":\"redacted\",\"id\":\"\",\"count\":1}"), "{}", lines[1]);
    assert_eq!(led.redact(Vec::<String>::new()).unwrap(), 0);
}

#[test]
fn for_turn_with_a_time_python_cannot_convert_has_nothing_due() {
    let tmp = tempfile::tempdir().unwrap();
    let led = ledger(tmp.path());
    // a commitment with neither a deadline nor a cue: its "" dates must not match an unknown today
    let bare = Card { deadline: None, cue: None, ..reminder() };
    led.new_card(bare, "take_on").unwrap();
    assert_eq!(led.for_turn(f64::NAN), "");
    assert_eq!(led.for_turn(1e300), "");
}
