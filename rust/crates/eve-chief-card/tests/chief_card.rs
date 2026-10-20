//! The chief's record (chief_card.py): the strict parse, the add-only door, the backbrief and the stored form.
//! Ported from the card half of bridge/tests/test_chief_record.py, test_chief_receipt.py's stored-result test and
//! test_link_chat.py's verbs control; the ledger half belongs to eve-chief-ledger.

use std::path::Path;

use eve_chief_card::*;
use serde_json::{json, Map, Value};

const SAID: &str = "find me a standing desk under $600 and order it";

fn obj(v: Value) -> Map<String, Value> {
    match v {
        Value::Object(m) => m,
        _ => panic!("not an object"),
    }
}

fn desk_args(over: Value) -> Value {
    let mut args = obj(json!({
        "kind": "job", "title": "Standing desk", "purpose": "a desk for the office",
        "end_state": "an order confirmation for one desk under $600",
        "done_checks": [{"kind": "read_links", "arg": 3}, {"kind": "cited_pick", "arg": null},
                        {"kind": "ui_seen", "arg": "order confirmed"}],
        "never": ["over $600"],
        "phases": [{"do": "research", "goal": "standing desks under $600 with reviews", "door": null},
                   {"do": "assess", "goal": "pick one", "door": null},
                   {"do": "act", "goal": "order the picked desk", "door": null}],
        "deadline": null, "cue": null, "firm": false, "question": null}));
    for (k, v) in obj(over) {
        args.insert(k, v);
    }
    Value::Object(args)
}

fn reminder_args(over: Value) -> Value {
    let mut args = obj(json!({
        "kind": "commitment", "title": "BOM export", "purpose": "the board order", "end_state": "the BOM is exported",
        "done_checks": [], "never": [], "phases": [], "deadline": "2026-10-02 17:00",
        "cue": {"at": "2026-10-01 10:00", "next_action": "export the BOM"}, "firm": true, "question": null}));
    for (k, v) in obj(over) {
        args.insert(k, v);
    }
    Value::Object(args)
}

fn parse_said(args: Value, said: &str) -> Result<Card, ParseError> {
    parse(&args, said, &ParseOptions::default())
}

fn card(over: Value) -> Card {
    let opts = ParseOptions { said_ref: "2026-09-29 10:02", ..ParseOptions::default() };
    parse(&desk_args(over), SAID, &opts).expect("the desk card parses")
}

fn refusal(out: Result<Card, ParseError>) -> String {
    match out {
        Err(ParseError::Refusal(r)) => r.reason,
        other => panic!("not a refusal: {other:?}"),
    }
}

fn revise_said(c: &Card, patch: Value, said: &str) -> Result<Card, ParseError> {
    revise(c, &obj(patch), said, &LIVE, 0.0, None)
}

fn desk_phases() -> Value {
    desk_args(json!({}))["phases"].clone()
}

// ---- the parse ---------------------------------------------------------------------------------

#[test]
fn test_a_whole_desk_job_parses_with_its_doors_budget_and_backbrief() {
    let c = card(json!({}));
    assert!(c.id.is_empty() && c.rev == 1 && c.status == "active" && c.waiting_for.is_none());
    let doors: Vec<(usize, &str, &str)> = c.phases.iter().map(|p| (p.n, p.r#do.as_str(), p.door.as_str())).collect();
    assert_eq!(doors, vec![(1, "research", "two_way"), (2, "assess", "two_way"), (3, "act", "one_way")]);
    assert!(c.phases[2].grounded); // the owner said "order"
    assert_eq!(c.budget, Budget { usd: DEFAULT_BUDGET_ACT_USD, wall_min: DEFAULT_BUDGET_ACT_MIN });
    let kinds: Vec<&str> = c.done_checks.iter().map(|x| x.kind.as_str()).collect();
    assert_eq!(kinds, vec!["read_links", "cited_pick", "ui_seen"]);
    assert_eq!(c.done_checks[0].arg, "3");
    assert_eq!(c.source.said_ref, "2026-09-29 10:02");
    let brief = backbrief(&c);
    assert!(brief.starts_with("On it: Standing desk. I intend to research standing desks under $600 with reviews"));
    assert!(brief.contains("order the picked desk after your Go") && brief.contains("Up to $1 and 30 min."));
    assert!(brief.contains("Done when an order confirmation for one desk under $600."));
    assert_eq!(
        brief,
        "On it: Standing desk. I intend to research standing desks under $600 with reviews, then pick one, then \
         order the picked desk after your Go. Done when an order confirmation for one desk under $600. Never: over \
         $600. Up to $1 and 30 min."
    );
}

#[test]
fn test_the_card_keeps_a_reference_to_the_words_never_the_words() {
    let c = card(json!({}));
    assert!(!serde_json::to_string(&to_json(&c)).unwrap().contains("standing desk under $600 and order it"));
}

#[test]
fn test_one_bad_phase_refuses_the_whole_card() {
    let mut phases = desk_phases().as_array().unwrap().clone();
    phases.push(json!({"do": "teleport", "goal": "x", "door": null}));
    let reason = refusal(parse_said(desk_args(json!({"phases": phases})), SAID));
    assert!(reason.contains("phase 4"), "{reason}");
    // control: without the bad phase the same card parses
    assert!(parse_said(desk_args(json!({})), SAID).is_ok());
}

#[test]
fn test_the_parse_is_strict() {
    let seven: Vec<Value> = (0..7).map(|i| json!({"do": "research", "goal": format!("desks {i}"), "door": null})).collect();
    let cases: Vec<(Value, &str)> = vec![
        (json!({"kind": "chore"}), "kind"),
        (json!({"title": "  "}), "title"),
        (json!({"phases": []}), "at least one phase"),
        (json!({"phases": [{"do": "research", "goal": "", "door": null}]}), "goal"),
        (json!({"phases": [{"do": "research", "goal": "desks", "door": "sideways"}]}), "door"),
        (json!({"phases": [{"do": "research", "goal": "desks", "door": null, "extra": 1}]}), "unknown field"),
        (json!({"phases": seven}), "at most 6"),
        (json!({"done_checks": [{"kind": "tests_green", "arg": ""}]}), "kind must be one of"),
        (json!({"done_checks": [{"kind": "read_links", "arg": 0}]}), "count"),
        (json!({"done_checks": [{"kind": "read_links", "arg": "three"}]}), "count"),
        (json!({"done_checks": [{"kind": "ui_seen", "arg": ""}]}), "ui_seen text"),
        (json!({"done_checks": [{"kind": "watch_armed", "arg": null}]}), "needs a watch phase"),
        (json!({"done_checks": [{"kind": "file_exists", "arg": "/etc/passwd"}]}), "home folder"),
        (json!({"done_checks": [{"kind": "file_exists", "arg": "~/../../etc/passwd"}]}), "home folder"),
        (json!({"deadline": "next friday"}), "not a date"),
        (json!({"cue": {"at": null, "next_action": "buy"}}), "a job has no cue"),
        (json!({"firm": "yes"}), "firm"),
        (json!({"surprise": true}), "unknown field"),
    ];
    for (over, words) in cases {
        let reason = refusal(parse_said(desk_args(over.clone()), SAID));
        assert!(reason.contains(words), "{over}: {reason}");
    }
}

#[test]
fn test_a_card_is_made_only_in_a_turn_with_the_owners_words() {
    assert!(refusal(parse_said(desk_args(json!({})), "")).contains("owner's words"));
    assert!(refusal(parse_said(desk_args(json!({})), "   ")).contains("owner's words"));
}

#[test]
fn test_a_commitment_has_no_phases_a_cue_and_only_the_owners_checks() {
    let c = parse_said(reminder_args(json!({})), "remind me Thursday to export the BOM, it's due Friday").unwrap();
    assert!(c.kind == "commitment" && c.phases.is_empty() && c.firm);
    let kinds: Vec<&str> = c.done_checks.iter().map(|x| x.kind.as_str()).collect();
    assert_eq!(kinds, vec!["guru_says_done"]); // the default check
    assert_eq!(c.cue, Some(Cue { at: Some("2026-10-01 10:00".into()), next_action: "export the BOM".into() }));
    assert_eq!(c.deadline.as_deref(), Some("2026-10-02 17:00"));
    assert_eq!(
        backbrief(&c),
        "Holding it: BOM export, due 2026-10-02 17:00. I'll remind you at your first break after 2026-10-01 10:00, \
         10:45 at the latest."
    );
    assert!(parse_said(reminder_args(json!({"phases": [{"do": "research", "goal": "x", "door": null}]})), "x").is_err());
    assert!(parse_said(reminder_args(json!({"cue": null})), "x").is_err());
    assert!(parse_said(reminder_args(json!({"done_checks": [{"kind": "ui_seen", "arg": "x"}]})), "x").is_err());
}

#[test]
fn test_a_missing_end_state_or_a_question_makes_the_card_wait() {
    let c = card(json!({"end_state": ""}));
    assert!(c.status == "waiting" && c.waiting_for.as_deref() == Some("question"));
    assert_eq!(missing(&c), vec!["end_state"]);
    let q = card(json!({"question": "Which room is it for?"}));
    assert!(q.status == "waiting" && missing(&q) == vec!["question"]);
    assert!(backbrief(&q).ends_with("Which room is it for?"));
}

#[test]
fn test_the_live_set_is_the_whole_offer() {
    let narrow = Live::new(&["research", "assess"], &["read_links", "cited_pick", "guru_says_done"]);
    let opts = ParseOptions { live: &narrow, ..ParseOptions::default() };
    let reason = refusal(parse(&desk_args(json!({})), SAID, &opts));
    assert!(reason.contains("phase 3"), "{reason}");
    let two_phases: Vec<Value> = desk_phases().as_array().unwrap()[..2].to_vec();
    let two_checks: Vec<Value> = desk_args(json!({}))["done_checks"].as_array().unwrap()[..2].to_vec();
    assert!(parse(&desk_args(json!({"phases": two_phases, "done_checks": two_checks})), SAID, &opts).is_ok());
    // only live names in the sets the code offers
    assert_eq!(PHASE_KINDS_LIVE, &["research", "assess", "act", "watch"]);
    assert_eq!(DONE_KINDS_LIVE, &["read_links", "cited_pick", "ui_seen", "watch_armed", "file_exists", "guru_says_done"]);
}

fn resolved(p: &Path) -> std::path::PathBuf {
    std::fs::canonicalize(p).unwrap()
}

#[test]
fn test_file_exists_takes_a_home_path() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir(&home).unwrap();
    let opts = ParseOptions { home: Some(&home), ..ParseOptions::default() };
    let args = desk_args(json!({"done_checks": [{"kind": "file_exists", "arg": "~/Desktop/bom.csv"}]}));
    assert!(parse(&args, SAID, &opts).is_ok());
    assert_eq!(home_path("~/Desktop/bom.csv", Some(&home)).unwrap(), Some(resolved(&home).join("Desktop/bom.csv")));
    assert_eq!(home_path("/tmp/x", Some(&home)).unwrap(), None);
    assert_eq!(home_path("~/../x", Some(&home)).unwrap(), None);
    let outside = tmp.path().join("elsewhere");
    std::fs::create_dir(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, home.join("link")).unwrap();
    assert_eq!(home_path("~/link/x", Some(&home)).unwrap(), None); // a symlink out is outside
    // the home folder itself proves nothing; a relative path is below home
    assert_eq!(home_path("~", Some(&home)).unwrap(), None);
    assert_eq!(home_path("  ", Some(&home)).unwrap(), None);
    assert_eq!(home_path("notes/a.md", Some(&home)).unwrap(), Some(resolved(&home).join("notes/a.md")));
}

// ---- the door ----------------------------------------------------------------------------------

#[test]
fn test_order_the_picked_desk_is_one_way_and_compare_desks_is_two_way() {
    assert_eq!(goal_door("order the picked desk"), "one_way");
    assert_eq!(goal_door("compare desks"), "two_way");
    assert_eq!(door("compare desks", false, &["GMAIL_SEND_EMAIL"], true), "one_way"); // a writing Composio call
    assert_eq!(door::<&str>("compare desks", false, &[], false), "one_way"); // an executor that cannot ask
    assert_eq!(door::<&str>("compare desks", true, &[], true), "one_way"); // the brain may raise
    assert_eq!(door("compare desks", false, &["  "], true), "two_way"); // a blank slug names no call
}

#[test]
fn test_the_brain_can_raise_a_door_but_never_lower_one() {
    let lowered = card(json!({"phases": [{"do": "act", "goal": "order the picked desk", "door": "two_way"}],
                              "done_checks": [{"kind": "ui_seen", "arg": "order confirmed"}]}));
    assert_eq!(lowered.phases[0].door, "one_way");
    let raised = card(json!({"phases": [{"do": "act", "goal": "open the desk page", "door": "one_way"}],
                             "done_checks": [{"kind": "ui_seen", "arg": "desk page"}]}));
    assert!(raised.phases[0].door == "one_way" && !raised.phases[0].grounded); // nothing the owner said
}

#[test]
fn test_grounded_needs_the_consequential_word_in_the_owners_words() {
    assert!(grounded("order the picked desk", SAID));
    assert!(!grounded("order the picked desk", "find me a standing desk under $600"));
    assert!(!grounded("compare desks", SAID)); // no verb names an act
    let c = parse_said(desk_args(json!({})), "find me a standing desk under $600").unwrap();
    assert!(c.phases[2].door == "one_way" && !c.phases[2].grounded);
}

#[test]
fn test_the_door_is_add_only_and_a_revision_cannot_lower_it() {
    let c = Card { id: "c1".into(), ..card(json!({})) };
    // the owner rewords the act so that no consequential word is left: the door stays one_way
    let patch = json!({"phases": [{"do": "research", "goal": "standing desks under $600 with reviews", "door": null},
                                  {"do": "assess", "goal": "pick one", "door": null},
                                  {"do": "act", "goal": "get the picked desk", "door": "two_way"}]});
    let new = revise_said(&c, patch, "change c1: get the picked desk").unwrap();
    assert_eq!(new.rev, 2);
    assert_eq!(new.phases[2].door, "one_way");
    // a phase moved into a one-way place inherits the door
    let moved = revise_said(
        &c,
        json!({"phases": [{"do": "research", "goal": "desks", "door": null},
                          {"do": "assess", "goal": "pick one", "door": null},
                          {"do": "research", "goal": "more desks", "door": null}],
               "done_checks": [{"kind": "read_links", "arg": 3}]}),
        "change c1",
    )
    .unwrap();
    assert_eq!(moved.phases[2].door, "one_way");
    // control: a phase that was never one-way is still two-way after the same revision
    assert!(new.phases[0].door == "two_way" && moved.phases[0].door == "two_way");
}

#[test]
fn test_a_revision_cancels_approvals_keeps_state_and_never_changes_the_kind() {
    let c = Card { id: "c1".into(), ..card(json!({})) };
    let ran = Phase {
        status: "ok".into(),
        executor: "search_router".into(),
        result: Some(PhaseResult::new("done", vec![Evidence::new("links", "https://a.example/1")])),
        ..c.phases[0].clone()
    };
    let approved = Phase { approval: Some(Approval::new("tok", 1, 5.0)), ..c.phases[2].clone() };
    let c = Card {
        phases: vec![ran.clone(), c.phases[1].clone(), approved],
        status: "waiting".into(),
        waiting_for: Some("go".into()),
        ..c
    };
    let new = revise(&c, &obj(json!({"never": ["over $550"]})), "change c1 never over $550", &LIVE, 9.0, None).unwrap();
    assert!(new.rev == 2 && new.never == vec!["over $550".to_string()]);
    assert!(new.phases[0].status == "ok" && new.phases[0].result == ran.result);
    assert!(new.phases[2].approval.is_none()); // the yes was for rev 1
    assert!(new.status == "waiting" && new.waiting_for.as_deref() == Some("go"));
    assert_eq!(new.updated, 9.0);
    assert!(revise_said(&c, json!({"kind": "commitment"}), "x").is_err());
    let done = Card { status: "done".into(), ..c.clone() };
    assert_eq!(refusal(revise_said(&done, json!({"never": []}), "x")), "c1 is done; reopen it first");
    let asked = Card { id: "c2".into(), ..card(json!({"question": "Which room?"})) };
    let answered = revise_said(&asked, json!({"question": null}), "office").unwrap();
    assert!(answered.status == "active" && answered.waiting_for.is_none());
}

#[test]
fn test_a_phase_inserted_at_the_front_keeps_every_phase_that_ran() {
    // Phases used to be matched by number: one inserted at the front put each later phase "in a new place", and
    // the research that had run went back to queued with its result gone (an act that ran could run again).
    let c = Card { id: "c1".into(), ..card(json!({})) };
    let ran = Phase {
        status: "ok".into(),
        executor: "search_router".into(),
        result: Some(PhaseResult::new("done", vec![Evidence::new("links", "https://a.example/1")])),
        ..c.phases[0].clone()
    };
    let acted = Phase {
        status: "ok".into(),
        executor: "browser".into(),
        result: Some(PhaseResult::new("done", vec![Evidence::new("ui_seen", "order confirmed")])),
        ..c.phases[2].clone()
    };
    let c = Card { phases: vec![ran.clone(), c.phases[1].clone(), acted.clone()], ..c };
    let front = json!({"do": "research", "goal": "measure the office wall", "door": null});
    let mut list = vec![front.clone()];
    list.extend(c.phases.iter().map(|p| json!({"do": p.r#do, "goal": p.goal, "door": null})));
    let patch = json!({"phases": list});
    let new = revise_said(&c, patch.clone(), "change c1: measure the wall first, then order the picked desk").unwrap();
    assert_eq!(new.phases.iter().map(|p| p.n).collect::<Vec<_>>(), vec![1, 2, 3, 4]);
    assert!(new.phases[0].goal == front["goal"] && new.phases[0].status == "queued" && new.phases[0].result.is_none());
    assert_eq!((&new.phases[1].goal, new.phases[1].status.as_str(), &new.phases[1].result), (&ran.goal, "ok", &ran.result));
    assert_eq!(new.phases[2].status, "queued"); // control: it never ran
    assert_eq!(
        (&new.phases[3].goal, new.phases[3].status.as_str(), &new.phases[3].result),
        (&acted.goal, "ok", &acted.result)
    );
    assert!(new.phases[3].door == "one_way" && new.phases[3].executor == "browser");
    // a phase deleted before them: the later ones keep their state and take the new numbers
    let kept: Vec<Value> =
        [&c.phases[0], &c.phases[2]].iter().map(|p| json!({"do": p.r#do, "goal": p.goal, "door": null})).collect();
    let gone = revise_said(
        &c,
        json!({"phases": kept,
               "done_checks": [{"kind": "read_links", "arg": 3}, {"kind": "ui_seen", "arg": "order confirmed"}]}),
        "change c1: skip the pick, order the picked desk",
    )
    .unwrap();
    assert_eq!(
        gone.phases.iter().map(|p| (p.n, p.status.as_str())).collect::<Vec<_>>(),
        vec![(1, "ok"), (2, "ok")]
    );
    assert_eq!(gone.phases[1].result, acted.result);
    // a running phase's result comes back by its number: a revision never moves it while it runs
    let running = Card {
        phases: vec![ran, Phase { status: "running".into(), ..c.phases[1].clone() }, acted],
        ..c.clone()
    };
    assert!(revise_said(&running, patch, "change c1: measure the wall first").is_err());
    assert!(revise_said(&running, json!({"never": ["over $550"]}), "change c1 never over $550").is_ok());
}

#[test]
fn test_a_stored_card_round_trips_and_a_bad_one_is_refused() {
    let c = Card { id: "c7".into(), ..card(json!({})) };
    assert_eq!(from_json(&to_json(&c)).unwrap(), c);
    let raw = to_json(&c);
    for (key, bad) in [("id", json!("7")), ("status", json!("finished")), ("rev", json!(0)), ("done_checks", json!([]))] {
        let mut broken = raw.clone();
        broken[key] = bad;
        assert!(from_json(&broken).is_err(), "{key}");
    }
    let mut broken = raw.clone();
    broken["phases"][0]["result"] = json!({"status": "done", "evidence": [{"kind": "rumour", "ref": "x"}]});
    assert!(from_json(&broken).is_err());
}

// ---- the reviewer's attacks after P4 (2026-09-29) ----------------------------------------------

#[test]
fn test_routine_acts_use_the_executors_live_confirmation_boundary() {
    // Codex can stop for a consequential action; merely using the Mac no longer needs a Go.
    let routine = card(json!({"phases": [{"do": "act", "goal": "organize the desk notes", "door": null}],
                              "done_checks": [{"kind": "guru_says_done", "arg": null}]}));
    assert_eq!(routine.phases[0].door, "two_way");
    let dangerous = card(json!({"phases": [{"do": "act", "goal": "delete the desk notes", "door": null}],
                                "done_checks": [{"kind": "guru_says_done", "arg": null}]}));
    assert_eq!(dangerous.phases[0].door, "one_way");
    let raised = card(json!({"phases": [{"do": "act", "goal": "open the desk page", "door": "one_way"}],
                             "done_checks": [{"kind": "ui_seen", "arg": "desk page"}]}));
    assert!(raised.phases[0].door == "one_way" && !raised.phases[0].grounded);
}

#[test]
fn test_a_stored_routine_two_way_act_keeps_its_door() {
    // Loading preserves routine two-way work and never lowers a stored one-way door.
    let c = Card {
        id: "c1".into(),
        ..card(json!({"phases": [{"do": "research", "goal": "desks", "door": null},
                                 {"do": "act", "goal": "open the desk page", "door": null}],
                      "done_checks": [{"kind": "guru_says_done", "arg": null}]}))
    };
    let mut raw = to_json(&c);
    raw["phases"][1]["door"] = json!("two_way");
    let reread: Value = serde_json::from_str(&serde_json::to_string(&raw).unwrap()).unwrap();
    let loaded = from_json(&reread).unwrap();
    assert_eq!(loaded.phases.iter().map(|p| p.door.as_str()).collect::<Vec<_>>(), vec!["two_way", "two_way"]);
    raw["phases"][1]["door"] = json!("one_way");
    assert_eq!(from_json(&raw).unwrap().phases[1].door, "one_way");
    // and a dangerous goal stored two-way loads one-way (the goal floor is reapplied)
    raw["phases"][1]["goal"] = json!("delete the desk notes");
    raw["phases"][1]["door"] = json!("two_way");
    assert_eq!(from_json(&raw).unwrap().phases[1].door, "one_way");
}

// ---- test_chief_receipt.py ---------------------------------------------------------------------

#[test]
fn test_a_stored_result_is_checked_strictly() {
    let read0 = "https://a.example/desk-1";
    let ok = result_from(&json!({"status": "done", "evidence": [{"kind": "links", "ref": read0}], "text": "",
                                 "usd": 0, "secs": 1}))
    .unwrap();
    assert_eq!(ok.evidence, vec![Evidence::new("links", read0)]);
    for bad in [
        json!({"status": "finished"}),
        json!({"status": "done", "evidence": [{"kind": "vibes", "ref": "x"}]}),
        json!({"status": "done", "usd": -1}),
        json!({"status": "done", "evidence": "x"}),
    ] {
        assert!(result_from(&bad).is_err(), "{bad}");
    }
}

// ---- test_link_chat.py -------------------------------------------------------------------------

#[test]
fn test_verbs_positive_control() {
    assert_eq!(verbs("order it").into_iter().collect::<Vec<_>>(), vec!["order".to_string()]);
    assert!(verbs("what's on my list").is_empty());
    let folded: Vec<String> = verbs("\u{17f}end it, SHUT DOWN, Log Out").into_iter().collect();
    assert_eq!(folded, vec!["log out", "send", "shut down"]);
}

// ---- parity details, checked against CPython 3.13 ------------------------------------------------

#[test]
fn refusal_messages_name_values_as_python_repr_does() {
    let cases: Vec<(Value, &str)> = vec![
        (json!({"kind": "chore"}), "kind must be one of ('job', 'commitment'), not 'chore'"),
        (json!({"kind": 1.5}), "kind must be one of ('job', 'commitment'), not 1.5"),
        (json!({"kind": null}), "kind must be one of ('job', 'commitment'), not None"),
        (json!({"phases": [{"do": "x"}]}), "phase 1: do must be one of ('research', 'assess', 'act', 'watch'), not 'x'"),
        (json!({"phases": [{"do": "research", "goal": "desks", "door": 0}]}),
         "phase 1: door must be one of ('two_way', 'one_way') or null, not 0"),
        (json!({"deadline": "2026-10-05 24:00"}),
         "deadline '2026-10-05 24:00' is not a date in the form %Y-%m-%d %H:%M or %Y-%m-%d"),
        (json!({"deadline": "2026-02-30"}), "deadline '2026-02-30' is not a date in the form %Y-%m-%d %H:%M or %Y-%m-%d"),
        (json!({"deadline": 5}), "deadline must be a string or null"),
        (json!({"firm": 2}), "firm must be true, false or null"),
        (json!({"cue": ""}), "a job has no cue: its phases are its next actions"),
        (json!({"done_checks": [{"kind": "cited_pick"}]}), "the cited_pick check needs a assess phase"),
        (json!({"zeta": 1, "alpha": 2}), "unknown field 'alpha'"),
        (json!({"phases": "x"}), "phases must be a list"),
        (json!({"never": ["ok", 3]}), "never must be a list of strings"),
        (json!({"question": 3}), "question must be a string or null"),
    ];
    for (over, want) in cases {
        let reason = refusal(parse_said(desk_args(json!({"phases": [{"do": "research", "goal": "desks", "door": null}],
                                                        "done_checks": [{"kind": "read_links", "arg": 3}]}))
            .as_object()
            .map(|base| {
                let mut m = base.clone();
                for (k, v) in obj(over.clone()) {
                    m.insert(k, v);
                }
                Value::Object(m)
            })
            .unwrap(), "hi"));
        assert_eq!(reason, want, "{over}");
    }
    let narrow = Live::new(&["research"], DONE_KINDS_LIVE);
    let opts = ParseOptions { live: &narrow, ..ParseOptions::default() };
    let act = desk_args(json!({"phases": [{"do": "act", "goal": "x", "door": null}], "done_checks": []}));
    assert_eq!(refusal(parse(&act, "hi", &opts)), "phase 1: do must be one of ('research',), not 'act'");
    let commit = reminder_args(json!({"done_checks": [{"kind": "ui_seen", "arg": "two words"}]}));
    assert_eq!(refusal(parse_said(commit, "hi")), "a commitment is closed only by ('file_exists', 'guru_says_done')");
    assert_eq!(refusal(parse(&json!([1]), "hi", &ParseOptions::default())), "take_on arguments must be an object");
}

#[test]
fn dates_are_read_as_strptime_reads_them() {
    let c = parse_said(desk_args(json!({"deadline": "2026-1-5 T9:5"})), "hi").unwrap();
    assert_eq!(c.deadline.as_deref(), Some("2026-01-05 09:05"));
    let c = parse_said(desk_args(json!({"deadline": "2026-10-05"})), "hi").unwrap();
    assert_eq!(c.deadline.as_deref(), Some("2026-10-05"));
    let c = parse_said(desk_args(json!({"deadline": "2028-02-29"})), "hi").unwrap();
    assert_eq!(c.deadline.as_deref(), Some("2028-02-29"));
    let c = parse_said(desk_args(json!({"deadline": ""})), "hi").unwrap();
    assert_eq!(c.deadline, None);
    for bad in ["2027-02-29", "0000-01-01", "2026-10-05 10:60", "2026-13-01", "26-10-05"] {
        assert!(parse_said(desk_args(json!({"deadline": bad})), "hi").is_err(), "{bad}");
    }
    let cue = reminder_args(json!({"cue": {"at": "2026-10-01", "next_action": "x"}}));
    assert!(refusal(parse_said(cue, "hi")).starts_with("cue.at '2026-10-01' is not a date in the form %Y-%m-%d %H:%M"));
    assert_eq!(at_time(Some("2026-10-01")).unwrap().to_string(), "2026-10-01 23:59:00");
    assert_eq!(at_time(Some("2026-10-01  9:05")).unwrap().to_string(), "2026-10-01 09:05:00");
    assert_eq!(at_time(Some("soon")), None);
    assert_eq!(at_time(Some("2026-10-01\u{1c}9:05")).unwrap().to_string(), "2026-10-01 09:05:00");
    assert_eq!(at_time(Some("2026-10-01T09:05")), None);
    // `\d` reads any script's digits, the month's `[0-9]` only ASCII ones
    for (raw, want) in [("\u{662}\u{660}\u{662}\u{666}-10-05", "2026-10-05"), ("2026-10- 5", "2026-10-05"),
                        ("2026-10-05  7:5", "2026-10-05 07:05"), ("2026-10-05 1:00\u{1c}", "2026-10-05 01:00")] {
        let c = parse_said(desk_args(json!({"deadline": raw})), "hi").unwrap();
        assert_eq!(c.deadline.as_deref(), Some(want), "{raw:?}");
    }
    assert!(parse_said(desk_args(json!({"deadline": "\u{662}\u{660}\u{662}\u{666}-\u{661}\u{660}-\u{660}\u{665}"})), "hi").is_err());
    assert_eq!(at_time(None), None);
}

#[test]
fn python_truthiness_and_equality_carry_over() {
    // `firm not in (None, True, False)`: 1 == True in Python
    assert!(parse_said(desk_args(json!({"firm": 1})), "hi").unwrap().firm);
    assert!(!parse_said(desk_args(json!({"firm": 0.0})), "hi").unwrap().firm);
    // a read_links count given as a string of digits, any script
    let c = parse_said(desk_args(json!({"done_checks": [{"kind": "read_links", "arg": " ٣ "}]})), "hi").unwrap();
    assert_eq!(c.done_checks[0].arg, "3");
    // the exceptions Python's parse lets out
    let sup = parse_said(desk_args(json!({"done_checks": [{"kind": "read_links", "arg": "²"}]})), "hi");
    assert_eq!(sup, Err(ParseError::ValueError(ValueError("invalid literal for int() with base 10: '²'".into()))));
    let nul = parse_said(desk_args(json!({"done_checks": [{"kind": "file_exists", "arg": "a\u{0}b"}]})), "hi");
    assert_eq!(nul, Err(ParseError::ValueError(ValueError("lstat: embedded null character in path".into()))));
}

#[test]
fn the_backbrief_reads_as_python_writes_it() {
    let research = card(json!({"phases": [{"do": "research", "goal": "find desks", "door": null},
                                          {"do": "watch", "goal": "desk prices under $500", "door": null}],
                               "done_checks": [{"kind": "guru_says_done", "arg": null}], "never": [],
                               "end_state": "a list..."}));
    assert_eq!(
        backbrief(&research),
        "On it: Standing desk. I intend to find desks, then watch desk prices under $500. Done when a list. Up to \
         $0.50 and 20 min."
    );
    let held = parse_said(reminder_args(json!({"deadline": null, "cue": {"at": null, "next_action": "x"}})), "hi").unwrap();
    assert_eq!(backbrief(&held), "Holding it: BOM export. I'll keep it on your list.");
    let pricey = Card { budget: Budget { usd: 12.5, wall_min: 45 }, ..research.clone() };
    assert!(backbrief(&pricey).ends_with("Up to $12 and 45 min."));
    let odd = Card { budget: Budget { usd: 2.005, wall_min: 1 }, ..research };
    assert!(backbrief(&odd).ends_with("Up to $2 and 1 min."));
}

#[test]
fn ids_and_stored_fields_follow_python() {
    assert_eq!(card_number("c12"), 12);
    assert_eq!(card_number("c12\n"), 12); // Python's `$` before a final newline
    assert_eq!(card_number("c0"), 0);
    assert_eq!(card_number("c١٢"), 0);
    assert_eq!(card_number("x"), 0);
    assert!(is_watch_id("w3") && is_watch_id("w3\n") && !is_watch_id("w03") && !is_watch_id("3"));
    let c = Card { id: "c3".into(), ..card(json!({})) };
    let mut raw = to_json(&c);
    raw["budget"]["wall_min"] = json!(1e300);
    assert!(from_json(&raw).is_ok());
    raw["created"] = json!(-1.0);
    assert_eq!(from_json(&raw).unwrap_err().0, "created is not a finite number >= 0.0");
    let mut raw = to_json(&c);
    raw["spent"]["usd"] = json!(true);
    assert_eq!(from_json(&raw).unwrap_err().0, "spent.usd is not a number >= 0.0");
    let mut raw = to_json(&c);
    raw["nudges"]["level"] = json!("loud");
    assert_eq!(from_json(&raw).unwrap_err().0, "c3: bad nudge level");
    let mut raw = to_json(&c);
    raw["firm"] = json!("no"); // bool("no") is True
    assert!(from_json(&raw).unwrap().firm);
    let mut raw = to_json(&c);
    raw["phases"][0]["result"] = json!({"status": "done", "evidence": []});
    assert_eq!(from_json(&raw).unwrap().phases[0].result, Some(PhaseResult::new("done", vec![])));
    // the stored keys in dataclass order
    let stored = to_json(&c);
    let keys: Vec<&String> = stored.as_object().unwrap().keys().collect::<Vec<_>>();
    assert_eq!(
        keys,
        [
            "id", "rev", "kind", "title", "purpose", "end_state", "done_checks", "never", "phases", "deadline", "cue",
            "firm", "question", "budget", "spent", "status", "waiting_for", "source", "nudges", "created", "updated"
        ]
    );
}

#[test]
fn reopen_and_to_args_follow_python() {
    let c = Card { id: "c1".into(), status: "done".into(), ..card(json!({})) };
    let c = Card {
        phases: c.phases.iter().map(|p| Phase { approval: Some(Approval::new("t", 1, 1.0)), ..p.clone() }).collect(),
        ..c
    };
    let r = reopen(&c, 5.0);
    assert!(r.rev == 2 && r.status == "active" && r.waiting_for.is_none() && r.updated == 5.0);
    assert!(r.phases.iter().all(|p| p.approval.is_none()));
    let args = to_args(&c);
    assert_eq!(args["question"], Value::Null);
    assert_eq!(args["done_checks"][0], json!({"kind": "read_links", "arg": "3"}));
    assert_eq!(args["phases"][2], json!({"do": "act", "goal": "order the picked desk", "door": "one_way"}));
    assert_eq!(Check::new("read_links", "3").describe(), "read 3 or more pages");
    assert_eq!(Check::new("ui_seen", "order confirmed").describe(), "'order confirmed' seen on screen");
    assert_eq!(Check::new("guru_says_done", "").describe(), "you say it is done");
}

#[test]
fn a_rev_at_the_integer_limit_does_not_overflow() {
    // A hand-edited store can hold rev 9223372036854775807; Python's rev + 1 grows past it, here it stays put
    // rather than panicking (debug) or wrapping to a negative rev that from_json would then refuse (release).
    let c = Card { id: "c1".into(), rev: i64::MAX, ..card(json!({})) };
    let loaded = from_json(&to_json(&c)).unwrap();
    assert_eq!(reopen(&loaded, 1.0).rev, i64::MAX);
    let revised = revise(&loaded, &obj(json!({"never": ["over $550"]})), "change c1", &LIVE, 2.0, None).unwrap();
    assert_eq!(revised.rev, i64::MAX);
    assert!(from_json(&to_json(&revised)).is_ok());
}
