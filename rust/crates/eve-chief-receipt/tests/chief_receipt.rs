//! The chief's oracles (chief_receipt.py): done means evidence the executor did not write.
//! Ported from bridge/tests/test_chief_receipt.py.
//!
//! Gate G2.1 of the chief design (2026-09-29): every done kind has one confirming fixture and one non-confirming
//! fixture; the executor's sentence alone is unverifiable; one unverifiable check makes the card unverified, never
//! done.

use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::path::Path;
use std::time::{Duration, UNIX_EPOCH};

use eve_chief_card as cc;
use eve_chief_card::{Card, Check, Evidence, ParseError, ParseOptions, PhaseResult};
use eve_chief_receipt as cr;
use eve_chief_receipt::{AgentRun, CheckOptions, Verdict};
use serde_json::{json, Value};

const READ: [&str; 3] = ["https://shop.example/desk-a", "https://review.example/desks", "https://shop.example/desk-b"];

fn job(checks: Value, phases: &[&str]) -> Card {
    let goal = |k: &str| match k {
        "research" => "standing desks under $600",
        "assess" => "pick one",
        "act" => "order the picked desk",
        _ => "the desk's price below $500",
    };
    let phases: Vec<Value> = phases.iter().map(|k| json!({"do": k, "goal": goal(k), "door": null})).collect();
    let args = json!({"kind": "job", "title": "Standing desk", "purpose": "", "end_state": "one desk ordered",
                      "done_checks": checks, "never": [], "phases": phases,
                      "deadline": null, "cue": null, "firm": false, "question": null});
    let out = cc::parse(&args, "find a standing desk and order it", &ParseOptions::default()).expect("the job parses");
    Card { id: "c1".into(), ..out }
}

fn with_results(card: &Card, results: &[(&str, PhaseResult)]) -> Card {
    let mut out = card.clone();
    for p in &mut out.phases {
        if let Some((_, r)) = results.iter().find(|(k, _)| *k == p.r#do) {
            p.result = Some(r.clone());
            p.status = "ok".into();
        }
    }
    out
}

fn links_with(urls: &[&str], status: &str) -> PhaseResult {
    PhaseResult { text: "an answer".into(), ..PhaseResult::new(status, urls.iter().map(|u| Evidence::new("links", u)).collect()) }
}

fn links(urls: &[&str]) -> PhaseResult {
    links_with(urls, "done")
}

fn ui_with(states: &[&str], status: &str, text: &str) -> PhaseResult {
    PhaseResult { text: text.into(), ..PhaseResult::new(status, states.iter().map(|s| Evidence::new("ui_text", s)).collect()) }
}

fn ui(states: &[&str]) -> PhaseResult {
    ui_with(states, "done", "")
}

fn one_with(card: &Card, opts: &CheckOptions<'_>) -> Verdict {
    let verdicts = cr::verify(card, opts).expect("no ValueError");
    assert_eq!(verdicts.len(), 1);
    verdicts.into_iter().next().expect("one verdict")
}

fn one(card: &Card) -> Verdict {
    one_with(card, &CheckOptions::default())
}

fn home(h: &Path) -> CheckOptions<'_> {
    CheckOptions { home: Some(h), ..CheckOptions::default() }
}

fn verify(card: &Card) -> Vec<Verdict> {
    cr::verify(card, &CheckOptions::default()).expect("no ValueError")
}

fn close(card: &Card) -> &'static str {
    cr::close(card, &CheckOptions::default()).expect("no ValueError")
}

fn outcomes(verdicts: &[Verdict]) -> Vec<&str> {
    verdicts.iter().map(|v| v.outcome.as_str()).collect()
}

fn set_mtime(path: &Path, at: f64) {
    let f = fs::File::options().write(true).open(path).expect("open");
    f.set_modified(UNIX_EPOCH + Duration::from_secs_f64(at)).expect("set mtime");
}

// ---- one confirming and one non-confirming fixture per done kind --------------------------------

#[test]
fn test_read_links() {
    let c = job(json!([{"kind": "read_links", "arg": 3}]), &["research"]);
    assert_eq!(one(&with_results(&c, &[("research", links(&READ))])).outcome, "confirmed");
    let short = one(&with_results(&c, &[("research", links(&READ[..2]))]));
    assert!(short.outcome == "failed" && short.why == "read 2 pages");
    let dot = format!("{}.", READ[0]);
    assert_eq!(one(&with_results(&c, &[("research", links(&[READ[0], READ[0], &dot]))])).outcome, "failed"); // distinct
    assert_eq!(
        one(&with_results(&c, &[("research", links(&["not a url", "ftp://x.example/a", READ[0]]))])).outcome,
        "failed"
    );
    assert_eq!(one(&c).outcome, "unverifiable"); // nothing ran yet
}

#[test]
fn test_cited_pick() {
    let c = job(json!([{"kind": "cited_pick", "arg": null}]), &["research", "assess"]);
    let good = with_results(&c, &[("research", links(&READ)), ("assess", links(&[READ[1]]))]);
    assert_eq!(one(&good).outcome, "confirmed");
    let unread =
        one(&with_results(&c, &[("research", links(&READ)), ("assess", links(&["https://elsewhere.example/desk"]))]));
    assert!(unread.outcome == "failed" && unread.why.contains("did not read"));
    assert_eq!(one(&with_results(&c, &[("research", links(&READ)), ("assess", links(&[]))])).outcome, "failed"); // cites nothing
    // a research phase that failed read nothing, so no pick can cite it
    assert_eq!(
        one(&with_results(&c, &[("research", links_with(&READ, "failed")), ("assess", links(&[READ[1]]))])).outcome,
        "failed"
    );
}

#[test]
fn test_ui_seen() {
    let c = job(json!([{"kind": "ui_seen", "arg": "Order confirmed"}]), &["act"]);
    let seen = one(&with_results(&c, &[("act", ui(&["Window: Checkout\nOrder   confirmed. Thank you!"]))]));
    assert_eq!(seen.outcome, "confirmed");
    assert_eq!(one(&with_results(&c, &[("act", ui(&["Window: Cart\n1 item"]))])).outcome, "unverifiable");
    assert_eq!(one(&with_results(&c, &[("act", ui(&[]))])).outcome, "unverifiable");
    assert_eq!(one(&with_results(&c, &[("act", ui_with(&["Order confirmed"], "refused", ""))])).outcome, "failed");
}

#[test]
fn test_watch_armed() {
    let c = job(json!([{"kind": "watch_armed", "arg": null}]), &["watch"]);
    let row = PhaseResult::new("done", vec![Evidence::new("watch_row", "w3")]);
    let armed = with_results(&c, &[("watch", row.clone())]);
    assert_eq!(one(&armed).outcome, "confirmed");
    let live: HashSet<String> = ["w3", "w4"].map(String::from).into();
    assert_eq!(one_with(&armed, &CheckOptions { live_watches: Some(&live), ..Default::default() }).outcome, "confirmed");
    let live: HashSet<String> = ["w4"].map(String::from).into();
    assert_eq!(one_with(&armed, &CheckOptions { live_watches: Some(&live), ..Default::default() }).outcome, "failed"); // removed since
    let bare = PhaseResult::new("done", vec![Evidence::new("watch_row", "3")]);
    assert_eq!(one(&with_results(&c, &[("watch", bare)])).outcome, "failed");
    assert_eq!(one(&with_results(&c, &[("watch", PhaseResult::new("done", vec![]))])).outcome, "failed");
}

#[test]
fn test_file_exists() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let tmp_path = tmp.path();
    let c = job(json!([{"kind": "file_exists", "arg": "~/Desktop/bom.csv"}]), &["research"]);
    assert_eq!(one_with(&c, &home(tmp_path)).outcome, "failed");
    fs::create_dir(tmp_path.join("Desktop")).expect("mkdir");
    fs::write(tmp_path.join("Desktop").join("bom.csv"), "x").expect("write");
    assert_eq!(one_with(&c, &home(tmp_path)).outcome, "confirmed");
    let outside = Card { done_checks: vec![Check::new("file_exists", "/etc/hosts")], ..c.clone() };
    assert_eq!(one_with(&outside, &home(tmp_path)).outcome, "failed"); // never outside home
}

#[test]
fn test_guru_says_done() {
    let c = job(json!([]), &["research"]);
    assert_eq!(c.done_checks.iter().map(|x| x.kind.as_str()).collect::<Vec<_>>(), ["guru_says_done"]);
    assert_eq!(one(&c).outcome, "unverifiable");
    let yes = one_with(&c, &CheckOptions { owner_done: true, ..Default::default() });
    assert!(yes.outcome == "confirmed" && yes.why == "your call");
}

#[test]
fn test_every_live_done_kind_has_an_oracle() {
    let covered: BTreeSet<&str> =
        ["read_links", "cited_pick", "ui_seen", "watch_armed", "file_exists", "guru_says_done"].into();
    assert_eq!(covered, cc::DONE_KINDS_LIVE.iter().copied().collect::<BTreeSet<_>>());
    for kind in cc::DONE_KINDS_LIVE {
        // none falls through to "no oracle"
        let arg = if *kind == "read_links" { "1" } else { "x" };
        let v = cr::check(&Check::new(kind, arg), &job(json!([]), &["research"]), &CheckOptions::default())
            .expect("no ValueError");
        assert_ne!(v.why, "no oracle for this check", "{kind}");
    }
}

// ---- the executor's word is never evidence ------------------------------------------------------

#[test]
fn test_the_executors_sentence_alone_is_unverifiable() {
    let c = job(json!([{"kind": "ui_seen", "arg": "order confirmed"}]), &["act"]);
    let claimed = with_results(&c, &[("act", ui_with(&[], "done", "I ordered the desk. Order confirmed, #12345."))]);
    assert_eq!(one(&claimed).outcome, "unverifiable");
    assert_eq!(close(&claimed), "unverified");
    // the research answer text names a link it never read: not a read link
    let r = job(json!([{"kind": "read_links", "arg": 1}]), &["research"]);
    let said_only = with_results(
        &r,
        &[("research", PhaseResult { text: "see https://shop.example/desk-a".into(), ..PhaseResult::new("done", vec![]) })],
    );
    assert!(one(&said_only).outcome == "failed" && cr::read_set(&said_only).is_empty());
}

#[test]
fn test_one_unverifiable_check_makes_the_card_unverified_never_done() {
    let c = job(
        json!([{"kind": "read_links", "arg": 3}, {"kind": "cited_pick", "arg": null},
               {"kind": "ui_seen", "arg": "order confirmed"}]),
        &["research", "assess", "act"],
    );
    let all_good =
        with_results(&c, &[("research", links(&READ)), ("assess", links(&[READ[0]])), ("act", ui(&["Order confirmed"]))]);
    let verdicts = verify(&all_good);
    assert!(outcomes(&verdicts) == ["confirmed"; 3] && close(&all_good) == "done");
    let one_short =
        with_results(&c, &[("research", links(&READ)), ("assess", links(&[READ[0]])), ("act", ui(&["Shipping details"]))]);
    assert_eq!(outcomes(&verify(&one_short)), ["confirmed", "confirmed", "unverifiable"]);
    assert_eq!(close(&one_short), "unverified");
    assert_eq!(cr::status_of(&[]), "unverified"); // no checks is not done
}

#[test]
fn test_a_handed_on_phase_is_unverifiable() {
    for status in ["handed_on", "needs_guru"] {
        let c = job(json!([{"kind": "ui_seen", "arg": "order placed"}]), &["act"]);
        assert_eq!(one(&with_results(&c, &[("act", ui_with(&["order placed"], status, ""))])).outcome, "unverifiable");
    }
}

#[test]
fn test_a_phase_that_did_not_happen_fails_its_check() {
    for status in ["failed", "refused", "over_budget"] {
        let c = job(json!([{"kind": "read_links", "arg": 1}]), &["research"]);
        let v = one(&with_results(&c, &[("research", links_with(&READ, status))]));
        assert!(v.outcome == "failed" && v.why.contains(&status.replace('_', " ")), "{status}: {v:?}");
    }
}

// ---- the receipt and the adapters ---------------------------------------------------------------

#[test]
fn test_the_receipt_is_composed_from_the_verdicts() {
    let c = job(
        json!([{"kind": "read_links", "arg": 3}, {"kind": "cited_pick", "arg": null},
               {"kind": "ui_seen", "arg": "Order confirmed"}]),
        &["research", "assess", "act"],
    );
    let four = [READ[0], READ[1], READ[2], "https://shop.example/desk-c"];
    let done =
        with_results(&c, &[("research", links(&four)), ("assess", links(&[READ[0]])), ("act", ui(&["Order confirmed"]))]);
    let done = Card { spent: cc::Spent { usd: 0.31, wall_s: 720.0 }, ..done };
    assert_eq!(
        cr::receipt(&done, &verify(&done)),
        "Done: Standing desk, 3 of 3 checks: read 4 pages (confirmed); the pick cites a page I read (confirmed); \
         'Order confirmed' seen on screen (confirmed). $0.31, 12 min."
    );
    let short = with_results(&c, &[("research", links(&READ)), ("assess", links(&[READ[0]])), ("act", ui(&["Cart"]))]);
    let text = cr::receipt(&short, &verify(&short));
    assert!(text.starts_with("Not confirmed: Standing desk, 2 of 3 checks") && text.ends_with("Did it go through?"));
    assert!(text.contains("'Order confirmed' not seen on screen (unverifiable)"));
}

#[test]
fn test_from_search_takes_the_sources_as_the_read_links() {
    let out = json!({"ok": true, "answer": "Desk A is best (https://made-up.example/x)", "cost_usd": 0.004,
                     "sources": [{"title": "A", "url": READ[0]}, {"title": "A again", "url": READ[0]},
                                 {"title": "bad", "url": "javascript:alert(1)"}, {"title": "B", "url": READ[1]}]});
    let r = cr::from_search(&out, 4.2);
    assert!(r.status == "done" && r.evidence.iter().map(|e| e.r#ref.as_str()).collect::<Vec<_>>() == [READ[0], READ[1]]);
    assert!(r.usd == 0.004 && r.secs == 4.2 && r.text.contains("made-up"));
    assert_eq!(cr::from_search(&json!({"ok": false, "reason": "the search did not answer it"}), 0.0).status, "failed");
}

#[test]
fn test_from_agent_and_from_watch() {
    let run = AgentRun { usd: 0.1, secs: 30.0, ..AgentRun::default() };
    let r = cr::from_agent("Done.", &[json!({"call_id": "a", "state": "Window: Shop\nOrder confirmed"}), json!({"state": " "})], &run);
    assert!(r.status == "done" && r.evidence.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>() == ["ui_text"]);
    let failed = AgentRun { ok: false, ..AgentRun::default() };
    assert_eq!(cr::from_agent("stopped", &[json!({"state": "Window: Shop"})], &failed).status, "failed");
    // an act that saw no screen is handed on, never done on its sentence (reviewer after P4, 2026-09-29)
    let act = AgentRun { act: true, ..AgentRun::default() };
    assert_eq!(cr::from_agent("Codex ended without a result.", &[], &act).status, "handed_on");
    assert_eq!(cr::from_agent("I ordered it.", &[json!({"state": " "})], &act).status, "handed_on");
    assert_eq!(cr::from_agent("Done.", &[json!({"state": "Window: Shop\nOrder confirmed"})], &act).status, "done");
    assert_eq!(cr::from_agent("Desk A is $549.", &[], &AgentRun::default()).status, "done"); // control: not an act
    assert_eq!(
        cr::from_watch(&json!({"ok": true, "id": "w12", "watching_for": "below $500"})).evidence,
        vec![Evidence::new("watch_row", "w12")]
    );
    assert_eq!(cr::from_watch(&json!({"ok": false, "reason": "no such symbol"})).status, "failed");
    assert_eq!(cr::from_watch(&json!({"ok": true, "id": "12"})).status, "failed");
}

// test_a_stored_result_is_checked_strictly tests chief_card.result_from; it is ported in eve-chief-card.

// ---- the reviewer's attacks after P3 (2026-09-29): each was reproduced first, then fixed ----------------------

#[test]
fn test_attack_a_failed_refused_or_skipped_step_makes_the_card_unverified() {
    let c = job(json!([{"kind": "read_links", "arg": 1}]), &["research", "act"]);
    let good = with_results(&c, &[("research", links(&READ))]);
    for status in ["failed", "refused", "skipped"] {
        let mut bad = good.clone();
        for p in &mut bad.phases {
            if p.r#do == "act" {
                p.status = status.into();
            }
        }
        let verdicts = verify(&bad);
        assert_eq!(outcomes(&verdicts), ["confirmed"]);
        assert!(close(&bad) == "unverified" && cr::card_status(&bad, &verdicts) == "unverified");
        let text = cr::receipt(&bad, &verdicts);
        assert!(text.starts_with("Not confirmed:") && text.contains(&format!("Step 2 (act) {status}.")), "{text}");
    }
    let mut ran = good.clone();
    for p in &mut ran.phases {
        p.status = "ok".into();
    }
    assert_eq!(close(&ran), "done"); // control
}

#[test]
fn test_attack_ui_seen_reads_only_the_screen_each_act_ended_on() {
    let c = job(json!([{"kind": "ui_seen", "arg": "order confirmed"}]), &["act"]);
    let before = "Window: Shop\nOrder confirmed within 2 days, or your money back"; // a product page, before the act
    let after = "Window: Checkout\nPayment declined";
    assert_eq!(one(&with_results(&c, &[("act", ui(&[before, after]))])).outcome, "unverifiable");
    assert_eq!(
        one(&with_results(&c, &[("act", ui(&[after, "Window: Checkout\nOrder confirmed #12"]))])).outcome,
        "confirmed"
    );
}

#[test]
fn test_attack_ui_seen_needs_at_least_two_words() {
    let args = json!({"kind": "job", "title": "Desk", "purpose": "", "end_state": "ordered",
                      "done_checks": [{"kind": "ui_seen", "arg": "order"}], "never": [],
                      "phases": [{"do": "act", "goal": "order the desk", "door": null}], "deadline": null, "cue": null,
                      "firm": false, "question": null});
    match cc::parse(&args, "order the desk", &ParseOptions::default()) {
        Err(ParseError::Refusal(r)) => assert!(r.reason.contains("two words"), "{}", r.reason),
        other => panic!("not a refusal: {other:?}"),
    }
    let c = job(json!([{"kind": "ui_seen", "arg": "order placed"}]), &["act"]);
    let one_word = Card { done_checks: vec![Check::new("ui_seen", "order")], ..c }; // a stored card, hand-edited
    assert_eq!(one(&with_results(&one_word, &[("act", ui(&["Window: Shop\nOrder now"]))])).outcome, "unverifiable");
}

#[test]
fn test_attack_file_exists_needs_a_file_below_home_made_or_changed_since_the_card() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let tmp_path = tmp.path();
    let args = json!({"kind": "job", "title": "Lamps", "purpose": "", "end_state": "read",
                      "done_checks": [{"kind": "file_exists", "arg": "~"}], "never": [],
                      "phases": [{"do": "research", "goal": "desk lamps", "door": null}], "deadline": null, "cue": null,
                      "firm": false, "question": null});
    let parsed = cc::parse(&args, "look up desk lamps", &ParseOptions { home: Some(tmp_path), ..ParseOptions::default() });
    assert!(matches!(parsed, Err(ParseError::Refusal(_))), "{parsed:?}");
    let c = job(json!([{"kind": "file_exists", "arg": "~/Desktop/bom.csv"}]), &["research"]);
    let c = Card { created: 1_790_000_000.0, ..c };
    let home_itself = Card { done_checks: vec![Check::new("file_exists", "~")], ..c.clone() };
    assert_eq!(one_with(&home_itself, &home(tmp_path)).outcome, "failed");
    let f = tmp_path.join("Desktop").join("bom.csv");
    fs::create_dir_all(&f).expect("mkdir"); // a folder is not a file
    assert_eq!(one_with(&c, &home(tmp_path)).outcome, "failed");
    fs::remove_dir(&f).expect("rmdir");
    fs::write(&f, "x").expect("write");
    set_mtime(&f, c.created - 60.0); // there before the card
    let old = one_with(&c, &home(tmp_path));
    assert!(old.outcome == "unverifiable" && old.why.contains("before"), "{old:?}");
    set_mtime(&f, c.created + 60.0);
    assert_eq!(one_with(&c, &home(tmp_path)).outcome, "confirmed"); // control
}

// ---- beyond the Python tests: the edges the Python code answers ----------------------------------

#[test]
fn a_hand_edited_count_is_read_as_python_int_reads_it() {
    let c = job(json!([{"kind": "read_links", "arg": 1}]), &["research"]);
    let ran = with_results(&c, &[("research", links(&READ))]);
    for (arg, outcome) in [("", "confirmed"), (" 3 ", "confirmed"), ("+2", "confirmed"), ("1_0", "failed"), ("-1", "confirmed")] {
        let edited = Card { done_checks: vec![Check::new("read_links", arg)], ..ran.clone() };
        assert_eq!(one(&edited).outcome, outcome, "{arg:?}");
    }
    let edited = Card { done_checks: vec![Check::new("read_links", "three")], ..ran.clone() };
    let err = cr::verify(&edited, &CheckOptions::default()).expect_err("int() raises");
    assert_eq!(err.0, "invalid literal for int() with base 10: 'three'");
    // the gate comes first: with nothing run there is no int() to raise
    let unrun = Card { done_checks: vec![Check::new("read_links", "three")], ..c };
    assert_eq!(one(&unrun).outcome, "unverifiable");
}

#[test]
fn a_link_is_trimmed_of_closing_punctuation_and_matched_whole() {
    let c = job(json!([{"kind": "read_links", "arg": 1}]), &["research"]);
    let r = with_results(&c, &[("research", links(&[" HTTPS://Shop.example/a?x=1!? ", "https://nodot/a", "http://a.b c"]))]);
    assert_eq!(cr::read_set(&r).into_iter().collect::<Vec<_>>(), ["HTTPS://Shop.example/a?x=1"]);
}

#[test]
fn the_gate_names_the_first_phase_that_did_not_happen() {
    let c = job(json!([{"kind": "read_links", "arg": 1}]), &["research", "research"]);
    let mut card = c.clone();
    card.phases[0].result = Some(PhaseResult::new("handed_on", vec![]));
    card.phases[1].result = Some(PhaseResult::new("over_budget", vec![]));
    let v = one(&card);
    assert!(v.outcome == "failed" && v.why == "the research phase over budget", "{v:?}");
    card.phases[1].result = Some(PhaseResult::new("needs_guru", vec![]));
    assert_eq!(one(&card).why, "the research phase was handed on");
}

#[test]
fn the_receipt_writes_time_and_money_as_python_formats_them() {
    let c = job(json!([]), &["research"]);
    let ran = Card { phases: c.phases.iter().map(|p| cc::Phase { status: "ok".into(), ..p.clone() }).collect(), ..c };
    let verdicts = vec![Verdict::new(&ran.done_checks[0], "confirmed", "your call")];
    let with = |usd: f64, wall_s: f64| cr::receipt(&Card { spent: cc::Spent { usd, wall_s }, ..ran.clone() }, &verdicts);
    assert_eq!(with(0.0, 0.0), "Done: Standing desk, 1 of 1 checks: your call (confirmed).");
    assert_eq!(with(0.125, 12.5), "Done: Standing desk, 1 of 1 checks: your call (confirmed). $0.12, 12 s.");
    assert_eq!(with(0.0, 29.9), "Done: Standing desk, 1 of 1 checks: your call (confirmed). $0.00, 30 s.");
    assert_eq!(with(1.0, 30.0), "Done: Standing desk, 1 of 1 checks: your call (confirmed). $1.00, 1 min.");
    assert_eq!(with(1.0, 90.0), "Done: Standing desk, 1 of 1 checks: your call (confirmed). $1.00, 2 min."); // 1.5 → 2
    assert_eq!(with(1.0, 150.0), "Done: Standing desk, 1 of 1 checks: your call (confirmed). $1.00, 2 min."); // 2.5 → 2
}

#[test]
fn the_adapters_take_python_values_as_python_does() {
    // a non-mapping result has no reason; secs never goes below zero
    let r = cr::from_search(&json!(["ok"]), -3.0);
    assert!(r.status == "failed" && r.text.is_empty() && r.secs == 0.0 && r.evidence.is_empty());
    let r = cr::from_search(&json!({"ok": 0, "reason": 404}), 1.0);
    assert!(r.status == "failed" && r.text == "404" && r.secs == 1.0);
    // a bool is not a cost; a source that is not a mapping gives no link; the answer is cut to its cap
    let long = "x".repeat(cc::MAX_RESULT_TEXT_CHARS + 5);
    let r = cr::from_search(&json!({"ok": 1, "cost_usd": true, "sources": ["https://a.example/x", {"url": READ[0]}],
                                    "answer": long}), f64::NAN);
    assert!(r.usd == 0.0 && r.secs == 0.0 && r.text.chars().count() == cc::MAX_RESULT_TEXT_CHARS);
    assert_eq!(r.evidence, vec![Evidence::new("links", READ[0])]);
    let r = cr::from_search(&json!({"ok": true, "sources": "https://a.example/x"}), 0.0);
    assert!(r.status == "done" && r.evidence.is_empty());
    // only the last MAX_EVIDENCE states are kept, oldest dropped
    let many: Vec<Value> = (0..cc::MAX_EVIDENCE + 2).map(|i| json!({"state": format!("Window: {i}")})).collect();
    let r = cr::from_agent("ok", &many, &AgentRun { usd: -1.0, ..AgentRun::default() });
    assert_eq!(r.evidence.len(), cc::MAX_EVIDENCE);
    assert_eq!(r.evidence[0].r#ref, "Window: 2");
    assert_eq!(r.usd, 0.0);
    // a state that is not text is read as Python's str() of it
    let r = cr::from_agent("ok", &[json!({"state": 5}), json!("Window: x")], &AgentRun::default());
    assert_eq!(r.evidence, vec![Evidence::new("ui_text", "5")]);
    // the watch row keeps its id; watching_for is cut to 200
    let r = cr::from_watch(&json!({"ok": true, "id": "w7", "watching_for": "y".repeat(300)}));
    assert!(r.status == "done" && r.text.len() == 200);
    assert!(cr::from_watch(&json!("w7")).status == "failed" && cr::from_watch(&json!(null)).text.is_empty());
}

#[test]
fn the_screen_is_matched_after_python_case_folding() {
    let c = job(json!([{"kind": "ui_seen", "arg": "Straße bestellt"}]), &["act"]);
    let v = one(&with_results(&c, &[("act", ui(&["Window: Shop\nSTRASSE   BESTELLT"]))]));
    assert_eq!(v.outcome, "confirmed");
}
