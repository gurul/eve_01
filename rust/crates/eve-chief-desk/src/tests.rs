//! Port of bridge/tests/test_chief_desk.py: the attention gate, backoff, expiry, and the briefs composed in code.
//!
//! Gate G2.2 of the chief design (2026-09-29): at 23:00 an item is batched and at 10:00 it goes now; the third
//! push of a day is batched; backoff doubles; the owner's own action cancels queued nudges; an expired decision
//! takes the option that does not act; the push count survives a reload; with no breakpoint in 45 minutes the
//! item moves to the next slot; any error falls back to batch.
//!
//! Gate G2.3: docs/stackchan/chief.md names every EVE_CHIEF* setting the code reads, and its documented defaults
//! equal the constants.

use std::fs;
use std::io::{Seek, SeekFrom, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::NaiveDate;
use eve_chief_card as cc;
use eve_chief_card::{Nudges, ParseOptions};
use eve_chief_ledger as cl;
use eve_chief_ledger::{Ledger, StateUpdate};
use regex::Regex;
use serde_json::{json, Value};

use super::*;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn chief_doc() -> String {
    fs::read_to_string(root().join("docs/stackchan/chief.md")).expect("docs/stackchan/chief.md")
}

fn at_day(hh: u32, mm: u32, day: u32) -> f64 {
    let naive = NaiveDate::from_ymd_opt(2026, 9, day).unwrap().and_hms_opt(hh, mm, 0).unwrap();
    py::naive_timestamp(&naive).unwrap()
}

fn at(hh: u32) -> f64 {
    at_day(hh, 0, 29)
}

fn desk(dir: &Path, mode: &str) -> Desk {
    Desk::new(Some(dir), DeskOptions { mode: Some(mode.into()), ..DeskOptions::default() })
}

/// A firm commitment due within the hour: the one unprompted nudge the desk may send.
fn urgent(t: f64, card_id: &str) -> Item {
    Item { queued_at: t, deadline_at: Some(t + 3600.0), firm: true, ..Item::new("nudge", card_id) }
}

fn urgent_with(t: f64, nudges: Nudges) -> Item {
    Item { nudges, ..urgent(t, "c1") }
}

fn queued(kind: &str, card_id: &str, t: f64) -> Item {
    Item { queued_at: t, ..Item::new(kind, card_id) }
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ---- quiet hours -------------------------------------------------------------------------------

#[test]
fn test_at_2300_an_item_is_batched_and_at_1000_it_goes_now() {
    let dir = tmp();
    let d = desk(dir.path(), "on");
    let (late, morning) = (at(23), at(10));
    d.breakpoint("task_end", Some(late)).unwrap();
    let r = d.offer(&urgent(late, "c1"), Some(late));
    assert_eq!((r.route, r.rule), ("batch", "quiet_hours"));
    assert_eq!(r.at, Some(at_day(8, 0, 30)));
    d.breakpoint("task_end", Some(morning)).unwrap();
    assert_eq!(d.offer(&urgent(morning, "c1"), Some(morning)), Route::new("now", "urgent_firm"));
    // a reminder the owner asked for and a Go wait out quiet hours too; a reply and a receipt do not
    assert_eq!(d.offer(&queued("reminder", "c2", late), Some(late)).rule, "quiet_hours");
    assert_eq!(d.offer(&queued("go", "c3", late), Some(late)).rule, "quiet_hours");
    assert_eq!(d.offer(&Item::new("reply", ""), Some(late)), Route::new("now", "reply"));
    assert_eq!(d.offer(&Item::new("receipt", "c3"), Some(late)), Route::new("now", "receipt"));
}

#[test]
fn test_quiet_hours_cross_midnight_and_a_daytime_window_works_too() {
    let m = Mandate::default();
    let q = |m: &Mandate, t: f64| in_quiet(m, t).unwrap();
    assert!(q(&m, at_day(22, 30, 29)) && q(&m, at(3)) && q(&m, at_day(7, 59, 29)));
    assert!(!q(&m, at(8)) && !q(&m, at_day(22, 29, 29)));
    let day = Mandate { quiet_start: "13:00".into(), quiet_end: "14:00".into(), ..Mandate::default() };
    assert!(q(&day, at_day(13, 30, 29)) && !q(&day, at(14)) && !q(&day, at(23)));
    assert_eq!(quiet_ends(&m, at(3)).unwrap(), at(8));
    assert_eq!(quiet_ends(&m, at(10)).unwrap(), at(10));
}

// ---- the daily budget ------------------------------------------------------------------------

#[test]
fn test_the_third_push_of_a_day_is_batched() {
    let dir = tmp();
    let d = desk(dir.path(), "on");
    let mut routes = Vec::new();
    for (i, hh) in [10, 12, 14].into_iter().enumerate() {
        let t = at(hh);
        d.breakpoint("relay_end", Some(t)).unwrap();
        routes.push(d.offer(&urgent(t, &format!("c{}", i + 1)), Some(t)));
    }
    assert_eq!(routes.iter().map(|r| r.route).collect::<Vec<_>>(), ["now", "now", "batch"]);
    assert_eq!(routes[2].rule, "push_budget");
    // reminders the owner asked for do not count and are not counted
    let t = at(15);
    d.breakpoint("task_end", Some(t)).unwrap();
    assert_eq!(d.offer(&queued("reminder", "c9", t), Some(t)), Route::new("now", "asked_reminder"));
    assert_eq!(d.pushes_today(Some(t)).unwrap(), 2);
    // a new day, a new budget
    let t = at_day(10, 0, 30);
    d.breakpoint("task_end", Some(t)).unwrap();
    assert_eq!(d.offer(&urgent(t, "c4"), Some(t)).route, "now");
}

#[test]
fn test_in_shadow_the_would_pushes_are_silent_and_counted() {
    let dir = tmp();
    let d = desk(dir.path(), "shadow");
    let mut out = Vec::new();
    for hh in [10, 12, 14] {
        let t = at(hh);
        d.breakpoint("task_end", Some(t)).unwrap();
        out.push(d.offer(&urgent(t, "c1"), Some(t)));
    }
    assert_eq!(
        out.iter().map(|r| (r.route, r.rule)).collect::<Vec<_>>(),
        [("silent", "urgent_firm"), ("silent", "urgent_firm"), ("batch", "push_budget")]
    );
    let off = desk(&dir.path().join("off"), "off");
    off.breakpoint("task_end", Some(at(10))).unwrap();
    assert_eq!(off.offer(&urgent(at(10), "c1"), Some(at(10))), Route::new("batch", "push_off"));
}

#[test]
fn test_the_push_count_survives_a_reload() {
    let dir = tmp();
    let d = desk(dir.path(), "on");
    for hh in [10, 11] {
        d.breakpoint("task_end", Some(at(hh))).unwrap();
        assert_eq!(d.offer(&urgent(at(hh), "c1"), Some(at(hh))).route, "now");
    }
    let again = desk(dir.path(), "on");
    assert_eq!(again.pushes_today(Some(at(12))).unwrap(), 2);
    again.breakpoint("task_end", Some(at(12))).unwrap();
    assert_eq!(again.offer(&urgent(at(12), "c1"), Some(at(12))), Route::new("batch", "push_budget"));
}

// ---- breakpoints -------------------------------------------------------------------------------

#[test]
fn test_unprompted_items_wait_for_a_breakpoint() {
    let dir = tmp();
    let d = desk(dir.path(), "on");
    let t = at(10);
    assert_eq!(d.offer(&urgent(t, "c1"), Some(t)), Route::new("batch", "await_breakpoint"));
    d.owner_said(Some(t));
    assert!(!d.at_breakpoint(t + 30.0));
    assert!(d.at_breakpoint(t + IDLE_SECS)); // 90 s after the owner's last word
    assert!(!d.at_breakpoint(t + 3.0 * 3600.0)); // hours later is not a moment
    d.breakpoint("wake_end", Some(t + 4000.0)).unwrap();
    assert!(d.at_breakpoint(t + 4000.0 + 60.0) && !d.at_breakpoint(t + 4000.0 + BREAKPOINT_WINDOW_SECS + 1.0));
    let err = d.breakpoint("lunch", Some(t)).unwrap_err();
    assert_eq!((err.name, err.message.as_str()), ("ValueError", "unknown breakpoint 'lunch'"));
}

#[test]
fn test_with_no_breakpoint_in_45_minutes_the_reminder_is_sent_anyway() {
    // owner, 2026-10-06: "just send it at 45 min" (it went to the next morning's brief)
    let dir = tmp();
    let d = desk(dir.path(), "on");
    let due = at(10);
    let waiting = d.offer(&queued("reminder", "c1", due), Some(due + 60.0));
    assert_eq!((waiting.route, waiting.rule), ("batch", "await_breakpoint"));
    assert_eq!(waiting.at, Some(due + REMINDER_WAIT_SECS));
    let late = d.offer(&queued("reminder", "c1", due), Some(due + REMINDER_WAIT_SECS));
    assert_eq!(late, Route { route: "now", rule: "reminder_waited", at: None });
    // quiet hours still hold it, however long it waited
    let night = at(23);
    assert_eq!(d.offer(&queued("reminder", "c1", night), Some(night + REMINDER_WAIT_SECS)).rule, "quiet_hours");
    // control: a breakpoint inside the 45 minutes sends it
    d.breakpoint("task_end", Some(due + 600.0)).unwrap();
    assert_eq!(d.offer(&queued("reminder", "c1", due), Some(due + 610.0)), Route::new("now", "asked_reminder"));
}

// ---- backoff, the owner's action, expiry ---------------------------------------------------------

#[test]
fn test_backoff_doubles_and_three_ignored_go_to_the_sheet() {
    let t = at(10);
    let mut n = Nudges::default();
    let mut gaps = Vec::new();
    for _ in 0..3 {
        n = Desk::ignored(&n, t);
        gaps.push(n.next_at.unwrap() - t);
    }
    assert_eq!(gaps, [NUDGE_GAP_SECS, 2.0 * NUDGE_GAP_SECS, 4.0 * NUDGE_GAP_SECS]);
    assert!(n.ignored == 3 && n.level == "sheet");
    let once = Nudges { ignored: 1, ..Nudges::default() };
    assert_eq!(Desk::sent(&once, t).next_at, Some(t + 2.0 * NUDGE_GAP_SECS));
    assert_eq!(Desk::sent(&once, t).sent, 1);
    let dir = tmp();
    let d = desk(dir.path(), "on");
    d.breakpoint("task_end", Some(t)).unwrap();
    let backing = Nudges { ignored: 1, next_at: Some(t + 60.0), ..Nudges::default() };
    assert_eq!(d.offer(&urgent_with(t, backing), Some(t)), Route::at("batch", "backoff", t + 60.0));
    assert_eq!(d.offer(&urgent_with(t, n), Some(t)), Route::new("batch", "sheet_only"));
}

#[test]
fn test_the_owners_own_action_cancels_queued_nudges() {
    let dir = tmp();
    let d = desk(dir.path(), "on");
    let t = at(10);
    d.breakpoint("task_end", Some(t + 100.0)).unwrap();
    d.acted("c1", Some(t + 50.0));
    assert_eq!(d.offer(&urgent(t, "c1"), Some(t + 100.0)), Route::new("drop", "owner_acted"));
    assert_eq!(d.offer(&queued("reminder", "c1", t), Some(t + 100.0)), Route::new("drop", "owner_acted"));
    assert_eq!(d.offer(&urgent(t, "c2"), Some(t + 100.0)).route, "now"); // control: another card goes
    assert_eq!(d.offer(&urgent(t + 60.0, "c1"), Some(t + 100.0)).route, "now"); // a nudge queued after the act goes
    assert_eq!(desk(dir.path(), "on").offer(&urgent(t, "c1"), Some(t + 100.0)).rule, "owner_acted"); // survives a reload
}

#[test]
fn test_only_a_firm_commitment_due_soon_may_nudge() {
    let dir = tmp();
    let d = desk(dir.path(), "on");
    let t = at(10);
    d.breakpoint("task_end", Some(t)).unwrap();
    let soft = Item { firm: false, ..urgent(t, "c1") };
    assert_eq!(d.offer(&soft, Some(t)).rule, "not_urgent");
    let far = Item { deadline_at: Some(t + 5.0 * 3600.0), ..urgent(t, "c1") };
    assert_eq!(d.offer(&far, Some(t)).rule, "not_urgent");
    let undated = Item { deadline_at: None, ..urgent(t, "c1") };
    assert_eq!(d.offer(&undated, Some(t)).rule, "not_urgent");
    let closed = Item { card_open: false, ..queued("go", "c1", t) };
    assert_eq!(d.offer(&closed, Some(t)), Route::new("drop", "card_closed"));
}

#[test]
fn test_an_expired_decision_takes_the_option_that_does_not_act() {
    let go = vec![Choice::new("Yes, order it", true), Choice::new("No", false)];
    assert_eq!(on_expiry(&go), Some(Choice::new("No", false)));
    let reversed: Vec<Choice> = go.iter().rev().cloned().collect();
    assert_eq!(on_expiry(&reversed), Some(Choice::new("No", false)));
    assert_eq!(on_expiry(&[Choice::new("Order A", true), Choice::new("Order B", true)]), None); // nothing, never an act
}

// ---- errors hold the message --------------------------------------------------------------------

#[test]
fn test_any_error_falls_back_to_batch() {
    let dir = tmp();
    let d = desk(dir.path(), "on");
    let t = at(10);
    d.breakpoint("task_end", Some(t)).unwrap();
    assert_eq!(d.offer(&Item::new("telegram_blast", "c1"), Some(t)), Route::new("batch", "error")); // an unknown kind

    d.break_probe_panics.store(true, Ordering::Relaxed);
    assert_eq!(d.offer(&urgent(t, "c1"), Some(t)), Route::new("batch", "error"));
    d.break_probe_panics.store(false, Ordering::Relaxed);
    // a push that cannot be logged is not sent: the log is a folder now
    fs::remove_file(d.path()).unwrap();
    fs::create_dir(d.path()).unwrap();
    assert_eq!(d.offer(&urgent(t, "c1"), Some(t)), Route::new("batch", "error"));
    assert_eq!(d.offer(&Item::new("reply", ""), Some(t)), Route::new("now", "reply")); // a reply is not a push
}

#[test]
fn a_time_the_clock_cannot_read_is_an_error_and_batched() {
    let dir = tmp();
    let d = desk(dir.path(), "on");
    for bad in [f64::NAN, 1e300, f64::INFINITY] {
        assert_eq!(d.offer(&urgent(bad, "c1"), Some(bad)), Route::new("batch", "error"));
    }
    assert!(in_quiet(&Mandate::default(), 1e300).is_err() && d.pushes_today(Some(f64::NAN)).is_err());
    // a mandate built by hand with a time that is not H:M breaks the gate, which holds the message
    let odd = Desk::new(
        Some(&dir.path().join("odd")),
        DeskOptions {
            mode: Some("on".into()),
            mandate: Some(Mandate { quiet_start: "late".into(), ..Mandate::default() }),
            ..DeskOptions::default()
        },
    );
    assert_eq!(odd.offer(&queued("go", "c1", at(10)), Some(at(10))), Route::new("batch", "error"));
    assert_eq!(odd.offer(&Item::new("receipt", "c1"), Some(at(10))), Route::new("now", "receipt"));
}

#[test]
fn test_a_mandate_that_does_not_parse_is_strict_and_said_once() {
    let dir = tmp();
    let file = dir.path().join(MANDATE_FILE);
    fs::write(&file, "quiet_start = \"21:00\"\npushes_per_day = 1\n").unwrap();
    let ok = desk(dir.path(), "on");
    assert_eq!(ok.mandate, Mandate { quiet_start: "21:00".into(), pushes_per_day: 1, ..Mandate::default() });
    assert_eq!(ok.take_problem(), "");
    let mut last = None;
    for bad in [
        "pushes_per_day = 99\n",
        "quiet_start = \"late\"\n",
        "volume = 3\n",
        "quiet_start = \n",
        "quiet_start = \"08:00\"\nquiet_end = \"08:00\"\n",
    ] {
        fs::write(&file, bad).unwrap();
        let d = desk(dir.path(), "on");
        assert!(d.mandate == *STRICT && d.mandate.pushes_per_day == 0, "{bad}");
        assert!(d.take_problem().contains("unprompted") && d.take_problem().is_empty(), "{bad}");
        last = Some(d);
    }
    let d = last.unwrap();
    let t = at(10);
    d.breakpoint("task_end", Some(t)).unwrap();
    assert_eq!(d.offer(&urgent(t, "c1"), Some(t)), Route::new("batch", "push_budget"));
    fs::remove_file(&file).unwrap();
    assert_eq!(desk(dir.path(), "on").mandate, Mandate::default());
}

#[test]
fn the_mandate_problem_lines_read_as_python_writes_them() {
    // python3: chief_desk.load_mandate(path) for each file
    let dir = tmp();
    let file = dir.path().join(MANDATE_FILE);
    let cases: [(&[u8], &str); 9] = [
        (b"quiet_start = \"8\"\n", "mandate.toml has a problem (time data '8' does not match format '%H:%M'), so I'm sending nothing unprompted until it's fixed."),
        (b"quiet_end = 8\n", "mandate.toml has a problem (not a time), so I'm sending nothing unprompted until it's fixed."),
        (b"a=1\r b=2", "mandate.toml has a problem (unknown setting 'a'), so I'm sending nothing unprompted until it's fixed."),
        (b"pushes_per_day = true\n", "mandate.toml has a problem (pushes_per_day must be 0 to 10), so I'm sending nothing unprompted until it's fixed."),
        (b"quiet_start = \"08:61\"\n", "mandate.toml has a problem (unconverted data remains: 1), so I'm sending nothing unprompted until it's fixed."),
        (b"quiet_start = \"21:00\"\nquiet_start = \"22:00\"\n", "I couldn't read mandate.toml (TOMLDecodeError), so I'm sending nothing unprompted."),
        ("\u{feff}quiet_start=\"21:00\"\n".as_bytes(), "I couldn't read mandate.toml (TOMLDecodeError), so I'm sending nothing unprompted."),
        (b"\xff", "I couldn't read mandate.toml (UnicodeDecodeError), so I'm sending nothing unprompted."),
        (b"quiet_start = \" 7:5 \"\nquiet_end = \"9:00\"\npushes_per_day = 0\n", ""),
    ];
    for (bytes, want) in cases {
        fs::write(&file, bytes).unwrap();
        let (m, problem) = load_mandate(&file);
        assert_eq!(problem, want, "{:?}", String::from_utf8_lossy(bytes));
        if want.is_empty() {
            assert_eq!(m, Mandate { quiet_start: "07:05".into(), quiet_end: "09:00".into(), pushes_per_day: 0 });
        } else {
            assert_eq!(m, *STRICT);
        }
    }
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    assert_eq!(
        load_mandate(&file).1,
        "I couldn't read mandate.toml (IsADirectoryError), so I'm sending nothing unprompted."
    );
    assert_eq!(load_mandate(&dir.path().join("none.toml")), (Mandate::default(), String::new()));
}

#[test]
fn test_push_mode_reads_the_switch() {
    let env = |v: &[(&str, &str)]| v.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect::<HashMap<_, _>>();
    assert_eq!(push_mode(Some(&env(&[]))), "shadow");
    assert_eq!(push_mode(Some(&env(&[("EVE_CHIEF_PUSH", "on")]))), "on");
    assert_eq!(push_mode(Some(&env(&[("EVE_CHIEF_PUSH", "1")]))), "on");
    assert_eq!(push_mode(Some(&env(&[("EVE_CHIEF_PUSH", "false")]))), "off");
    assert_eq!(push_mode(Some(&env(&[("EVE_CHIEF_PUSH", "loud")]))), "shadow");
    assert_eq!(push_mode(Some(&env(&[("EVE_CHIEF_PUSH", " YES ")]))), "on");
    assert_eq!(push_mode(Some(&env(&[("EVE_CHIEF_PUSH", "")]))), "shadow");
    // a mode passed in wins over the switch; one that is not a mode reads it
    let dir = tmp();
    let opts = |mode: &str| DeskOptions {
        mode: Some(mode.into()),
        environ: Some(env(&[("EVE_CHIEF_PUSH", "off")])),
        ..DeskOptions::default()
    };
    assert_eq!(Desk::new(Some(dir.path()), opts("on")).mode, "on");
    assert_eq!(Desk::new(Some(dir.path()), opts("loud")).mode, "off");
}

#[test]
fn test_attention_lines_carry_ids_and_rules_never_text() {
    let dir = tmp();
    let d = desk(dir.path(), "on");
    d.offer(&Item::new("reply", "c1"), Some(at(10)));
    let text = fs::read_to_string(d.path()).unwrap();
    let line = text.lines().next().unwrap();
    let row: Value = serde_json::from_str(line).unwrap();
    let keys: Vec<&str> = row.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys, ["t", "kind", "id", "route", "rule", "mode", "unprompted"]);
}

#[test]
fn attention_lines_are_the_bytes_python_writes() {
    // python3: Desk(d, mode="on").offer(Item("telegram_blastéé_long_kind", "c1234567890123456789"),
    // 1790000000.12345); .acted("c9é", 1790000001.0005)
    let dir = tmp();
    let d = desk(dir.path(), "on");
    let r = d.offer(&Item::new("telegram_blast\u{e9}\u{e9}_long_kind", "c1234567890123456789"), Some(1_790_000_000.123_45));
    assert_eq!(r, Route::new("batch", "error"));
    d.acted("c9\u{e9}", Some(1_790_000_001.000_5));
    assert_eq!(
        fs::read_to_string(d.path()).unwrap(),
        "{\"t\":1790000000.123,\"kind\":\"telegram_blast\\u00e9\\u00e9\",\"id\":\"c123456789012345\",\"route\":\"batch\",\"rule\":\"error\",\"mode\":\"on\",\"unprompted\":false}\n\
         {\"t\":1790000001.0,\"kind\":\"acted\",\"id\":\"c9\\u00e9\"}\n"
    );
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(fs::metadata(d.path()).unwrap().permissions().mode() & 0o777, 0o600);
}

// ---- the briefs, composed in code -----------------------------------------------------------------

fn parse(args: Value, said: &str) -> cc::Card {
    match cc::parse(&args, said, &ParseOptions::default()) {
        Ok(card) => card,
        Err(e) => panic!("{e:?}"),
    }
}

fn job(title: &str, over: &[(&str, Value)]) -> cc::Card {
    let mut args = json!({"kind": "job", "title": title, "purpose": "", "end_state": "a desk is ordered",
        "done_checks": [{"kind": "ui_seen", "arg": "order placed"}], "never": [],
        "phases": [{"do": "research", "goal": "desks", "door": null},
                   {"do": "act", "goal": "order the picked desk", "door": null}],
        "deadline": null, "cue": null, "firm": false, "question": null});
    for (k, v) in over {
        args[*k] = v.clone();
    }
    parse(args, "order a desk")
}

fn reminder(title: &str, cue: &str, deadline: &str) -> cc::Card {
    parse(
        json!({"kind": "commitment", "title": title, "purpose": "", "end_state": "done", "done_checks": [],
            "never": [], "phases": [], "deadline": deadline, "cue": {"at": cue, "next_action": title},
            "firm": true, "question": null}),
        "remind me",
    )
}

struct TestClock(Arc<AtomicU64>);

impl TestClock {
    fn new(t: f64) -> Self {
        TestClock(Arc::new(AtomicU64::new(t.to_bits())))
    }

    fn set(&self, t: f64) {
        self.0.store(t.to_bits(), Ordering::SeqCst);
    }

    fn ledger(&self, dir: &Path) -> Ledger {
        let cell = self.0.clone();
        Ledger::with_clock(Some(dir), Arc::new(move || f64::from_bits(cell.load(Ordering::SeqCst))))
    }
}

#[test]
fn test_the_first_brief_is_three_lines_at_most_from_the_ledger() {
    let dir = tmp();
    let clock = TestClock::new(at(9));
    let led = clock.ledger(dir.path());
    assert_eq!(first_brief(&led, at(9)), "");
    let desk_job = led.new_card(job("Standing desk", &[]), "take_on").unwrap();
    led.update(&desk_job.id, "go asked", StateUpdate::new().status("waiting").waiting_for(Some("go"))).unwrap();
    let bom = led.new_card(reminder("BOM export", "2026-09-29 10:00", "2026-09-30 17:00"), "take_on").unwrap();
    let old = led.new_card(job("Monitor arm", &[]), "take_on").unwrap();
    led.update(&old.id, "closed", StateUpdate::new().status("done")).unwrap();
    led.new_card(cc::Card { status: "proposed".into(), ..job("Desk lamp", &[]) }, "take_on").unwrap();
    let brief = first_brief(&led, at_day(9, 30, 29));
    assert_eq!(
        brief.lines().collect::<Vec<_>>(),
        [
            format!("Today's one thing: BOM export ({}), due 2026-09-30 17:00.", bom.id),
            format!("Waiting on you: {} your Go (order the picked desk).", desk_job.id),
            format!("Closed since yesterday: Monitor arm ({}, done).", old.id),
        ]
    );
}

#[test]
fn the_first_brief_names_what_is_proposed_when_there_is_room() {
    let dir = tmp();
    let clock = TestClock::new(at(9));
    let led = clock.ledger(dir.path());
    let q = led.new_card(job("Desk mat", &[]), "take_on").unwrap();
    led.update(&q.id, "asked", StateUpdate::new().status("waiting").waiting_for(Some("question"))).unwrap();
    let b = led.new_card(job("Chair", &[]), "take_on").unwrap();
    led.update(&b.id, "over", StateUpdate::new().status("waiting").waiting_for(Some("budget"))).unwrap();
    let lamp = led.new_card(cc::Card { status: "proposed".into(), ..job("Desk lamp", &[]) }, "take_on").unwrap();
    assert_eq!(
        first_brief(&led, at(10)),
        format!(
            "Waiting on you: {} your answer (Desk mat); {} a budget raise (Chair).\nProposed: Desk lamp ({}).",
            q.id, b.id, lamp.id
        )
    );
}

#[test]
fn test_the_evening_close_and_the_weekly_sheet() {
    let dir = tmp();
    let clock = TestClock::new(at_day(12, 0, 20));
    let led = clock.ledger(dir.path());
    let stale = led.new_card(job("Old errand", &[]), "take_on").unwrap();
    clock.set(at(12));
    let done = led.new_card(job("Standing desk", &[]), "take_on").unwrap();
    led.update(&done.id, "receipt", StateUpdate::new().status("done")).unwrap();
    let slipped = led.new_card(reminder("BOM export", "2026-09-28 10:00", "2026-09-29 17:00"), "take_on").unwrap();
    let tomorrow = led.new_card(reminder("Call the bank", "2026-09-30 09:00", "2026-09-30 12:00"), "take_on").unwrap();
    let ignored = led.new_card(reminder("File taxes", "2026-10-05 09:00", "2026-10-06 12:00"), "take_on").unwrap();
    let nudges = Nudges { sent: 3, ignored: 3, next_at: None, level: "sheet".into() };
    led.update(&ignored.id, "ignored", StateUpdate::new().nudges(nudges)).unwrap();
    let unsure = led.new_card(job("Desk mat", &[]), "take_on").unwrap();
    led.update(&unsure.id, "receipt", StateUpdate::new().status("unverified")).unwrap();
    let close = evening_close(&led, at(21)).unwrap();
    assert_eq!(
        close.lines().collect::<Vec<_>>(),
        [
            format!("Closed today: Standing desk ({}: 'order placed' seen on screen).", done.id),
            format!("Slipped: BOM export ({}, due 2026-09-29 17:00).", slipped.id),
            format!("Tomorrow first: Call the bank ({})?", tomorrow.id),
        ]
    );
    let sheet = weekly_sheet(&led, at_day(18, 0, 29));
    assert_eq!(sheet.ids, [stale.id.clone(), ignored.id.clone(), unsure.id.clone()]);
    assert!(sheet.text.contains(&format!("{} Old errand: untouched for 9 days", stale.id)));
    assert!(sheet.text.contains(&format!("{} File taxes: reminder ignored 3 times", ignored.id)));
    assert!(sheet.text.contains(&format!("{} Desk mat: not confirmed", unsure.id)));
    assert!(!sheet.ids.contains(&tomorrow.id));
    assert!(evening_close(&led, f64::NAN).is_err());
}

#[test]
fn a_long_line_is_cut_at_a_word() {
    // python3: chief_desk._fit(["word " * 50])
    let cut = fit(&["word ".repeat(50)], 200);
    assert_eq!(cut, [format!("{}…", ["word"; 39].join(" "))]);
    assert_eq!(fit(&["x".repeat(201)], 200), [format!("{}…", "x".repeat(199))]);
    assert_eq!(fit(&["x".repeat(200)], 200), ["x".repeat(200)]);
}

// ---- docs (G2.3) --------------------------------------------------------------------------------

fn knob_row(text: &str, knob: &str) -> String {
    let re = Regex::new(&format!(r"(?m)^\| `{}` \| `([^`]*)` \|", regex::escape(knob))).unwrap();
    re.captures(text).unwrap_or_else(|| panic!("no Knobs row for {knob}"))[1].to_string()
}

fn python_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            python_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "py") {
            out.push(path);
        }
    }
}

#[test]
fn test_docs_name_every_chief_setting_the_code_reads_with_its_default() {
    // The settings whose defaults live in this crate or its upstream crates; EVE_CHIEF, EVE_CHIEF_ASSESS_* and
    // EVE_CHIEF_REPLAN default in chief.py and chief_replan.py, checked with those crates. Every setting the code
    // reads must still have its Knobs row.
    let documented: HashMap<&str, &str> = [("EVE_CHIEF_DIR", cl::DEFAULT_DIR), ("EVE_CHIEF_PUSH", DEFAULT_PUSH_MODE)].into();
    let knob = Regex::new(r"\bEVE_CHIEF[A-Z0-9_]*").unwrap();
    let mut files = Vec::new();
    python_sources(&root().join("bridge/src/eve_bridge"), &mut files);
    let mut read = std::collections::BTreeSet::new();
    for path in files {
        let text = fs::read_to_string(&path).unwrap();
        read.extend(knob.find_iter(&text).map(|m| m.as_str().to_string()));
    }
    assert!(!read.is_empty(), "positive control: the chief's own modules read EVE_CHIEF settings");
    assert!(read.contains("EVE_CHIEF_DIR") && read.contains("EVE_CHIEF_PUSH"));
    let text = chief_doc();
    for k in &read {
        let row = knob_row(&text, k);
        if let Some(want) = documented.get(k.as_str()) {
            assert_eq!(row, *want, "{k}");
        }
    }
}

#[test]
fn test_docs_mandate_defaults_equal_the_constants() {
    let text = chief_doc();
    assert!(knob_row(&text, "quiet_start") == QUIET_START && QUIET_START == "22:30");
    assert!(knob_row(&text, "quiet_end") == QUIET_END && QUIET_END == "08:00");
    assert!(knob_row(&text, "pushes_per_day") == PUSHES_PER_DAY.to_string() && PUSHES_PER_DAY == 2);
    let mut keys = MANDATE_KEYS.to_vec();
    keys.sort();
    assert_eq!(keys, ["pushes_per_day", "quiet_end", "quiet_start"]);
    for word in ["$0.50 and 20 min", "$1 and 30 min", "21:00", "Sunday 18:00", "45 minutes", "90 seconds"] {
        assert!(text.contains(word), "{word}");
    }
    assert!(cc::DEFAULT_BUDGET_READ_USD == 0.50 && cc::DEFAULT_BUDGET_READ_MIN == 20);
    assert!(cc::DEFAULT_BUDGET_ACT_USD == 1.00 && cc::DEFAULT_BUDGET_ACT_MIN == 30);
    assert!(EVENING_CLOSE_AT == "21:00" && WEEKLY_SHEET_AT == "18:00" && WEEKLY_SHEET_DAY == 6);
}

#[test]
fn test_docs_name_the_live_sets_and_carry_no_owner_data() {
    let text = chief_doc();
    for kind in cc::DONE_KINDS_LIVE.iter().chain(cc::RESULT_STATUSES).chain(cc::STATUSES) {
        assert!(text.contains(&format!("`{kind}")), "{kind}");
    }
    for kind in cc::PHASE_KINDS_LIVE {
        assert!(text.contains(&format!("`{kind}`")), "{kind}");
    }
    for event in cl::EVENT_KINDS {
        assert!(text.contains(event), "{event}");
    }
    let private = Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}|/Users/|\b[0-9]{9,}\b").unwrap();
    assert!(!private.is_match(&text));
    // positive control: the same pattern finds a planted address
    let email = Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}").unwrap();
    assert!(email.is_match("write to someone@example.org"));
    assert!(!text.replace("guru_says_done", "").replace("needs_guru", "").contains("Guru"));
}

// ---- the reviewer's attacks after P3 (2026-09-29): each was reproduced first, then fixed ----------------------

#[test]
fn test_attack_a_bad_row_in_the_attention_log_is_skipped() {
    for row in [
        r#"{"kind":"acted","id":"c1","t":"soon"}"#,
        r#"{"kind":"acted","id":"c1","t":1e999}"#,
        r#"{"kind":"acted","id":"c1","t":[1]}"#,
        r#"{"t":"x","unprompted":true,"route":"now"}"#,
    ] {
        let dir = tmp();
        fs::write(dir.path().join(ATTENTION_FILE), format!("{row}\n{{\"kind\":\"acted\",\"id\":\"c2\",\"t\":5.0}}\n"))
            .unwrap();
        let d = desk(dir.path(), "on");
        assert_eq!(d.pushes_today(Some(at(10))).unwrap(), 0, "{row}");
        // control
        assert_eq!(d.offer(&queued("reminder", "c2", 4.0), Some(at(10))), Route::new("drop", "owner_acted"), "{row}");
    }
}

#[test]
fn test_attack_a_budget_warning_is_unprompted() {
    let dir = tmp();
    let (late, morning) = (at(23), at(10));
    let on = desk(&dir.path().join("on"), "on");
    on.breakpoint("task_end", Some(late)).unwrap();
    assert_eq!(on.offer(&queued("warn", "c1", late), Some(late)).rule, "quiet_hours");
    assert_eq!(desk(&dir.path().join("off"), "off").offer(&Item::new("warn", "c1"), Some(morning)), Route::new("batch", "push_off"));
    on.breakpoint("task_end", Some(morning)).unwrap();
    assert_eq!(on.offer(&queued("warn", "c1", morning), Some(morning)), Route::new("now", "budget_warning"));
    assert_eq!(on.pushes_today(Some(morning)).unwrap(), 1); // it counts against the day's budget
}

#[test]
fn the_evening_close_and_the_weekly_sheet_are_unprompted_rules() {
    let dir = tmp();
    let d = desk(dir.path(), "shadow");
    let t = at(21);
    d.breakpoint("idle", Some(t)).unwrap();
    assert_eq!(d.offer(&Item::new("close", ""), Some(t)), Route::new("silent", "evening_close"));
    assert_eq!(d.offer(&Item::new("sheet", ""), Some(t)), Route::new("silent", "weekly_sheet"));
    assert_eq!(d.offer(&Item::new("sheet", ""), Some(t)), Route::new("batch", "push_budget"));
}

#[test]
fn test_pushes_today_parses_only_new_attention_lines() {
    // Review D6: pushes_today re-parsed all of attention.jsonl on every offer. Python counts json.loads calls;
    // here a line already read is changed in place (same file, same length) and must not be read again.
    let row = |t: f64, i: &str, r: &str| format!("{{\"t\":{t},\"kind\":\"verdict\",\"id\":\"{i}\",\"route\":\"{r}\",\"unprompted\":true}}\n");
    let dir = tmp();
    let path = dir.path().join(ATTENTION_FILE);
    fs::write(&path, row(at(9), "c1", "now")).unwrap();
    let d = desk(dir.path(), "on");
    assert_eq!(d.pushes_today(Some(at(10))).unwrap(), 1);
    let mut f = fs::OpenOptions::new().write(true).open(&path).unwrap();
    f.seek(SeekFrom::Start(0)).unwrap();
    f.write_all(row(at(9), "c1", "nop").as_bytes()).unwrap();
    drop(f);
    assert_eq!(d.pushes_today(Some(at(10))).unwrap(), 1);
    fs::OpenOptions::new().append(true).open(&path).unwrap().write_all(row(at_day(9, 30, 29), "c2", "silent").as_bytes()).unwrap();
    assert_eq!(d.pushes_today(Some(at(10))).unwrap(), 2);
    // control: a fresh reader parses the changed line and does not count it
    assert_eq!(desk(dir.path(), "on").pushes_today(Some(at(10))).unwrap(), 1);
}

#[test]
fn a_desk_with_no_folder_reads_eve_chief_dir() {
    let dir = tmp();
    let environ: HashMap<String, String> =
        [("EVE_CHIEF_DIR".to_string(), dir.path().to_string_lossy().into_owned())].into();
    let d = Desk::new(None, DeskOptions { environ: Some(environ), ..DeskOptions::default() });
    assert_eq!(d.dir(), dir.path());
    assert_eq!(d.mode, "shadow");
    assert_eq!(d.path(), dir.path().join(ATTENTION_FILE));
}

#[test]
fn the_desk_reads_the_clock_when_no_time_is_given() {
    let dir = tmp();
    let clock = Arc::new(AtomicU64::new(at(10).to_bits()));
    let c = clock.clone();
    let d = Desk::new(
        Some(dir.path()),
        DeskOptions {
            mode: Some("on".into()),
            clock: Some(Arc::new(move || f64::from_bits(c.load(Ordering::SeqCst)))),
            ..DeskOptions::default()
        },
    );
    d.breakpoint("task_end", None).unwrap();
    assert_eq!(d.offer(&urgent(at(10), "c1"), None), Route::new("now", "urgent_firm"));
    assert_eq!(d.pushes_today(None).unwrap(), 1);
    clock.store(at(23).to_bits(), Ordering::SeqCst);
    assert_eq!(d.offer(&Item::new("go", "c1"), None).rule, "quiet_hours");
}
