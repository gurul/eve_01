//! records.py's tests (bridge/tests/test_records.py), the record switches (test_memory_records.py), and the parts of
//! other test files that exercise this module: records' git guard (test_toolchain.py), the switch defaults
//! (test_memory_verdict.py), the talking section (test_small_talk.py) and the dream's client (test_model_memory_roles,
//! test_spend_wiring, test_privacy_guards).

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{NaiveDate, NaiveDateTime};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::recall::RecallConfig;
use crate::records::*;
use crate::testutil::*;

const DAY: &str = "2026-09-20";
const DAY_TEXT: &str = "12:01 texted Owner: ramen for lunch now, pasta is over\n12:02 texted eve: Noted, ramen it is.";

fn dining() -> Record {
    let mut r = Record::new(
        "dining",
        "preference",
        &["food", "lunch", "restaurants", "takeout"],
        &["Loves pasta (said 2026-09-15).", "Usual lunch spot is [[cafe-nero]] (said 2026-09-18)."],
    );
    r.updated = "2026-09-18".into();
    r
}

fn cafe() -> Record {
    Record::new("cafe-nero", "place", &["cafe", "coffee place", "nero"], &["The coffee place on the main street (said 2026-09-18)."])
}

fn at(y: i32, m: u32, d: u32, h: u32, min: u32) -> Option<NaiveDateTime> {
    NaiveDate::from_ymd_opt(y, m, d).and_then(|d| d.and_hms_opt(h, min, 0))
}

fn result_with(over: &[(&str, Value)]) -> Value {
    let mut base = json!({
        "records": [{"id": "dining", "type": "preference", "aliases": ["food", "lunch", "ramen"],
                     "facts": ["Now prefers ramen for lunch (changed 2026-09-20; was pasta).",
                               "Usual lunch spot is [[cafe-nero]] (said 2026-09-18)."]},
                    {"id": "ramen-bar", "type": "place", "aliases": ["ramen place"],
                     "facts": ["Where lunch comes from now (said 2026-09-20)."]}],
        "forget": [],
        "profile": {"life_context": ["Works on a desk robot called eve."],
                    "acting": ["Order lunch without asking; ask before anything over $50."],
                    "talking": ["Short texts, no lists."]},
        "journal": {"title": "Ramen replaces pasta", "happened": ["12:01 lunch talk by text"],
                    "learned": ["Prefers ramen now"], "open": ["Find the ramen bar's hours"],
                    "corrections": ["Lunch: pasta became ramen (2026-09-20)"]}});
    for (k, v) in over {
        base[*k] = v.clone();
    }
    base
}

fn result() -> Value {
    result_with(&[])
}

fn journal_with(over: &[(&str, Value)]) -> Value {
    let mut j = result()["journal"].clone();
    for (k, v) in over {
        j[*k] = v.clone();
    }
    j
}

fn dream(cfg: &RecallConfig, v: Value) -> Option<DayChange> {
    with_env(&[], || reconcile_day(cfg, &FakeClient::one(v), DAY, DAY_TEXT))
}

fn ids(recs: &Records) -> Vec<String> {
    let mut v: Vec<String> = recs.keys().cloned().collect();
    v.sort();
    v
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn lines(text: &str) -> Vec<String> {
    text.lines().map(|s| s.to_string()).collect()
}

// ---- the file format --------------------------------------------------------------------------

#[test]
fn test_a_record_round_trips_and_a_hand_edited_one_still_parses() {
    assert_eq!(parse_record(&render_record(&dining())), Some(dining()));
    assert_eq!(dining().links(), vec!["cafe-nero".to_string()]);
    let hand = "---\nid: dining\nType: Preference\naliases: [ food , 'lunch' ]\n---\n\nsome prose\n\n* Loves pasta.\n- Hates kale\n";
    let rec = parse_record(hand).unwrap();
    assert_eq!(rec.aliases, vec!["food", "lunch"]);
    assert_eq!(rec.facts, vec!["Loves pasta.", "Hates kale"]);
    assert_eq!(rec.r#type, "preference");
    for bad in ["", "no frontmatter", "---\nid: Has Spaces\n---\n- x", "---\ntype: person\n---\n- no id"] {
        assert!(parse_record(bad).is_none(), "{bad}");
    }
}

#[test]
fn test_the_records_folder_is_the_configs_and_load_skips_what_is_not_a_record() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    assert_eq!(rdir(&cfg), cfg.records_dir());
    assert_eq!(cfg.records_dir(), cfg.store.join("records"));
    seed(&cfg, &[&dining(), &cafe()]);
    put(&cfg, "profile.md", "---\nid: profile\n---\n# hi\n");
    put(&cfg, "imposter.md", &render_record(&dining()));
    star(&cfg, "my sister lives by the sea", None);
    assert_eq!(ids(&load_records(&cfg)), vec!["cafe-nero", "dining"]);
    assert!(load_records(&RecallConfig::new(t.path().join("none"), t.path().join("none"))).is_empty());
}

// ---- stars -----------------------------------------------------------------------------------------

#[test]
fn test_a_star_is_one_dated_line_in_starred_md_and_saying_it_twice_is_one() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    let line = star(&cfg, "  my name is Sam. ", at(2026, 9, 11, 15, 0));
    assert_eq!(line.as_deref(), Some("- My name is Sam (2026-09-11)"));
    assert_eq!(star(&cfg, "MY NAME IS SAM!", at(2026, 9, 12, 9, 0)), line); // case-insensitive
    assert_eq!(star(&cfg, "my sister is Ana", at(2026, 9, 12, 1, 30)).as_deref(), Some("- My sister is Ana (2026-09-11)"));
    assert_eq!(star(&cfg, "ok", None), None);
    assert_eq!(star(&cfg, "", None), None);
    let text = read(rdir(&cfg).join("starred.md"));
    assert_eq!(text.matches("My name is Sam").count(), 1);
    assert!(text.starts_with("# Starred"));
    assert_eq!(stars(&cfg, MAX_STARS), vec!["My name is Sam", "My sister is Ana"]); // text only, newest last
    assert_eq!(stars(&cfg, 1), vec!["My sister is Ana"]);
    assert_eq!(mode(&rdir(&cfg).join("starred.md")), 0o600);
    assert_eq!(mode(&rdir(&cfg)), 0o700);
    assert!(!cfg.store.join("HIGHLIGHTS.md").exists());
}

#[test]
fn test_a_hand_typed_or_migrated_star_line_reads_too() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    put(&cfg, "starred.md", "# Starred\n\n- ★ Allergic to peanuts (2026-09-01)\n- Likes jazz\n");
    assert_eq!(stars(&cfg, MAX_STARS), vec!["Allergic to peanuts", "Likes jazz"]);
    assert_eq!(star(&cfg, "allergic to peanuts", None).as_deref(), Some("- Allergic to peanuts (2026-09-01)"));
}

#[test]
fn test_a_star_closes_an_open_last_line_and_never_follows_a_link() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    put(&cfg, "starred.md", "# Starred\n\n- Likes jazz");
    assert!(star(&cfg, "likes tea", at(2026, 9, 11, 12, 0)).is_some());
    assert_eq!(read(rdir(&cfg).join("starred.md")), "# Starred\n\n- Likes jazz\n- Likes tea (2026-09-11)\n");
    let other = cfg_at(&t.path().join("other"));
    std::fs::create_dir_all(rdir(&other)).unwrap();
    let target = t.path().join("elsewhere.md");
    std::fs::write(&target, "").unwrap();
    std::os::unix::fs::symlink(&target, rdir(&other).join("starred.md")).unwrap();
    assert_eq!(star(&other, "likes tea", None), None); // O_NOFOLLOW: refused, logged
    assert_eq!(read(&target), "");
}

// ---- the profile -------------------------------------------------------------------------------------

#[test]
fn test_a_star_newer_than_the_profile_is_on_top_of_it_verbatim() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    assert_eq!(profile(&cfg), ""); // no stars, no page: nothing
    star(&cfg, "my name is Sam", at(2026, 9, 11, 12, 0));
    assert!(profile(&cfg).starts_with(FRESH_STARS_HEADING)); // no page yet: the stars are it
    dream(&cfg, result());
    let prof = profile(&cfg);
    assert!(!prof.contains("They asked you to remember") && !prof.contains("My name is Sam")); // the page has read it
    star(&cfg, "my sister is Ana", at(2026, 9, 22, 12, 0));
    let prof = RecordsReader::new(cfg.clone(), None).profile();
    assert!(prof.starts_with("---\n")); // the frontmatter stays on top
    assert!(prof.find(FRESH_STARS_HEADING).unwrap() < prof.find("## Life context").unwrap()); // stars above every section
    assert!(prof.contains("- My sister is Ana (2026-09-22)"));
    assert!(!prof.contains("My name is Sam") && prof.contains("## Life context"));
    put(&cfg, "profile.md", &format!("# The owner\n{}", "x\n".repeat(5000)));
    let n = profile(&cfg).chars().count();
    assert!(0 < n && n <= MAX_PROFILE_CHARS);
}

#[test]
fn test_the_voice_profile_drops_the_index_and_stays_small() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&dining(), &cafe()]);
    dream(&cfg, result());
    let full = profile(&cfg);
    assert_eq!(INDEX_HEADING, "## Records you can search or read by id"); // the heading it strips
    assert!(full.contains(INDEX_HEADING) && full.contains("- cafe-nero (place)")); // positive control
    let voice = profile_for_voice(&cfg);
    assert!(!voice.contains(INDEX_HEADING) && !voice.contains("cafe-nero (place)"));
    assert!(voice.contains("## Life context") && voice.contains("Short texts") && !voice.contains("id: profile"));
    star(&cfg, "my name is Sam", at(2026, 9, 25, 12, 0));
    assert!(profile_for_voice(&cfg).starts_with(FRESH_STARS_HEADING));
    put(&cfg, "profile.md", &format!("# The owner\n{}", "y\n".repeat(5000)));
    let n = profile_for_voice(&cfg).chars().count();
    assert!(0 < n && n <= MAX_VOICE_PROFILE_CHARS);
}

// ---- don't mention this again --------------------------------------------------------------------------

/// Every file in the records folder except hidden.md itself (and git's own), as bytes.
fn source_bytes(cfg: &RecallConfig) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            let p = e.path();
            if p.file_name().is_some_and(|n| n == ".git") {
                continue;
            }
            if p.is_dir() {
                walk(&p, root, out);
            } else if p.file_name().is_some_and(|n| n != HIDDEN_FILE) {
                out.push((p.strip_prefix(root).unwrap().to_string_lossy().into_owned(), std::fs::read(&p).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(&rdir(cfg), &rdir(cfg), &mut out);
    out.sort();
    out
}

#[test]
fn test_hide_is_one_dated_private_line_deduped_and_unhide_takes_it_back() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    assert!(hidden(&cfg).is_empty() && hidden_match(&cfg).is_none());
    let line = hide(&cfg, "  the  dentist. ", at(2026, 10, 9, 15, 0));
    assert_eq!(line.as_deref(), Some("- the dentist (2026-10-09)"));
    assert_eq!(hide(&cfg, "THE DENTIST", at(2026, 10, 10, 9, 0)), line); // same words: one line
    assert_eq!(hide(&cfg, "\"ana's birthday\"", at(2026, 10, 10, 1, 0)).as_deref(), Some("- \"ana's birthday\" (2026-10-09)"));
    assert_eq!(hide(&cfg, "", None), None);
    assert_eq!(hide(&cfg, " . ", None), None);
    assert_eq!(hide(&cfg, "a", None), None);
    let path = rdir(&cfg).join(HIDDEN_FILE);
    let text = read(&path);
    assert!(text.starts_with("# Hidden") && text.matches("dentist").count() == 1);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(hidden(&cfg), vec!["the dentist", "\"ana's birthday\""]);
    assert!(unhide(&cfg, "The Dentist."));
    assert!(!unhide(&cfg, "the dentist") && !unhide(&cfg, ""));
    assert_eq!(hidden(&cfg), vec!["\"ana's birthday\""]);
    assert_eq!(mode(&path), 0o600);
    assert!(!load_records(&cfg).contains_key("hidden")); // never a record
}

struct FakeIndex(Result<Vec<Value>, String>);

impl MeaningSearch for FakeIndex {
    fn search(&self, _query: &str) -> anyhow::Result<Vec<Value>> {
        self.0.clone().map_err(|e| anyhow::anyhow!(e))
    }
}

#[test]
fn test_hidden_words_leave_the_profile_and_the_searches_but_nothing_is_deleted() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&dining(), &cafe()]);
    dream(&cfg, result());
    star(&cfg, "my sister is Ana", at(2026, 9, 22, 12, 0));
    star(&cfg, "my dentist is Dr Lee", at(2026, 9, 22, 12, 0));
    let (full, voice) = (profile(&cfg), profile_for_voice(&cfg));
    // positive controls: every line about to be hidden is there first
    assert!(full.contains("Order lunch without asking") && voice.contains("Order lunch without asking"));
    assert!(full.contains("Dr Lee") && voice.contains("Dr Lee") && full.contains("My sister is Ana"));
    assert_eq!(search(&load_records(&cfg), "coffee place", MAX_HITS, None)[0]["id"], "cafe-nero");
    let index = FakeIndex(Ok(vec![
        json!({"text": "Dentist is on main street", "day": "2026-09-20", "channel": ""}),
        json!({"text": "Likes jazz", "day": "2026-09-19", "channel": ""}),
    ]));
    let reader = RecordsReader::new(cfg.clone(), Some(&index));
    assert_eq!(reader.search("street")["recalled"].as_array().unwrap().len(), 2);
    let before = source_bytes(&cfg);

    hide(&cfg, "order lunch", None); // every word, any case, anywhere in the line
    hide(&cfg, "dentist", None);
    hide(&cfg, "\"the coffee place on\"", None); // a quoted phrase as written
    let (full, voice) = (profile(&cfg), profile_for_voice(&cfg));
    assert!(!full.contains("Order lunch") && !voice.contains("Order lunch"));
    assert!(!full.contains("Dr Lee") && !voice.contains("Dr Lee"));
    assert!(full.contains("My sister is Ana") && voice.contains("My sister is Ana")); // the rest stays
    assert!(full.contains("## Life context") && full.contains("Works on a desk robot") && voice.contains("Short texts"));
    assert!(full.find(FRESH_STARS_HEADING).unwrap() < full.find("## Life context").unwrap());
    let pred = hidden_match(&cfg).unwrap();
    let hits = search(&load_records(&cfg), "coffee place", MAX_HITS, Some(&*pred));
    assert!(hits.iter().all(|h| !h["line"].as_str().unwrap().contains("The coffee place on the main street")));
    assert_eq!(search(&load_records(&cfg), "coffee place", MAX_HITS, None)[0]["id"], "cafe-nero"); // without hide: as before
    assert_eq!(reader.search("street")["recalled"], json!(["(2026-09-19) Likes jazz"])); // the dentist row is left out
    let got = reader.get("cafe-nero");
    assert!(!got.get("record").and_then(Value::as_str).unwrap_or("").contains("coffee place on the main street"));
    assert_eq!(source_bytes(&cfg), before); // nothing deleted anywhere
    assert!(read(rdir(&cfg).join("cafe-nero.md")).contains("The coffee place on the main street"));

    unhide(&cfg, "dentist"); // unhidden: back on the next read
    assert!(profile(&cfg).contains("Dr Lee") && !profile(&cfg).contains("Order lunch"));
}

#[test]
fn test_a_star_wholly_hidden_drops_the_fresh_heading_and_a_hide_survives_a_forget_of_other_words() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    star(&cfg, "my dentist is Dr Lee", at(2026, 9, 22, 12, 0));
    assert!(profile(&cfg).starts_with(FRESH_STARS_HEADING));
    hide(&cfg, "dr lee", None);
    assert_eq!(profile(&cfg), "");
    assert_eq!(profile_for_voice(&cfg), "");
    with_env(&[], || {
        assert_eq!(forget_lines(&cfg, &|ln: &str| ln.contains("jazz"), false), 0);
        assert_eq!(hidden(&cfg), vec!["dr lee"]); // hidden.md is not a record
        assert_eq!(forget_lines(&cfg, &|ln: &str| ln.to_lowercase().contains("lee"), false), 2); // the star and the hide
    });
    assert!(hidden(&cfg).is_empty() && rdir(&cfg).join(HIDDEN_FILE).exists());
}

// ---- what the brains may do --------------------------------------------------------------------------

#[test]
fn test_search_is_keyword_over_aliases_and_lines_and_aliases_count_more() {
    let mut recs = Records::new();
    recs.insert("dining".into(), dining());
    recs.insert("cafe-nero".into(), cafe());
    let hits = search(&recs, "where do I get lunch?", MAX_HITS, None);
    assert!(hits[0]["id"] == "dining" && hits[0]["line"].as_str().unwrap().contains("lunch spot"));
    assert_eq!(search(&recs, "coffee", MAX_HITS, None)[0]["id"], "cafe-nero");
    assert!(search(&recs, "main street", MAX_HITS, None)[0]["line"].as_str().unwrap().starts_with("The coffee place"));
    assert!(search(&recs, "", MAX_HITS, None).is_empty() && search(&recs, "quantum", MAX_HITS, None).is_empty());
    assert!(search(&recs, "the", 1, None).len() <= 1);
}

#[test]
fn test_the_reader_re_reads_disk_so_an_owner_edit_counts_at_once() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&dining()]);
    let reader = RecordsReader::new(cfg.clone(), None);
    assert_eq!(reader.profile(), "");
    assert_eq!(reader.search("pasta")["hits"][0]["id"], "dining");
    put(&cfg, "dining.md", &render_record(&Record::new("dining", "preference", &["food"], &["Went off pasta (said 2026-09-21)."])));
    assert!(reader.search("pasta")["hits"][0]["line"].as_str().unwrap().starts_with("Went off"));
    assert!(reader.get("dining")["record"].as_str().unwrap().starts_with("---\nid: dining"));
    assert_eq!(reader.get("nope")["ok"], false);
    assert_eq!(reader.get("nope")["reason"], "no record 'nope'");
    assert_eq!(reader.search("quantum"), json!({"ok": true, "hits": [], "note": "nothing matched"}));
}

#[test]
fn test_recalled_memories_carry_their_day_and_channel() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    let index = FakeIndex(Ok(vec![
        json!({"text": "Sister lives by the sea", "day": "2026-09-20", "channel": "voice"}),
        json!({"text": "Likes jazz", "day": "2026-09-19", "channel": ""}),
        json!({"text": "Undated", "day": "", "channel": ""}),
        json!("not a row"),
    ]));
    let out = RecordsReader::new(cfg.clone(), Some(&index)).search("sister");
    assert_eq!(out["recalled"], json!(["(2026-09-20, voice) Sister lives by the sea", "(2026-09-19) Likes jazz", "Undated"]));
    assert!(out.get("note").is_none());
    let broken = FakeIndex(Err("boom".into()));
    assert_eq!(RecordsReader::new(cfg, Some(&broken)).search("x")["note"], "nothing matched");
}

#[test]
fn test_latest_day_is_the_newest_journal_and_its_title() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    assert_eq!(latest_day(&cfg), None);
    dream(&cfg, result());
    let quiet = result_with(&[("journal", journal_with(&[("title", json!("Quiet day"))]))]);
    with_env(&[], || reconcile_day(&cfg, &FakeClient::one(quiet), "2026-09-21", DAY_TEXT));
    std::fs::write(rdir(&cfg).join("days").join("notes.md"), "# not a day\n").unwrap();
    assert_eq!(latest_day(&cfg), Some(("2026-09-21".to_string(), "Quiet day".to_string())));
}

// ---- the dream: reconcile one day ----------------------------------------------------------------------

#[test]
fn test_a_day_is_dreamt_into_records_a_profile_a_journal_and_one_commit() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&dining(), &cafe()]);
    star(&cfg, "my name is Sam", at(2026, 9, 11, 12, 0));
    let client = FakeClient::one(result());
    let change = with_env(&[], || reconcile_day(&cfg, &client, DAY, DAY_TEXT)).unwrap();
    assert_eq!((change.changed, change.created, change.corrections), (1, 1, 1));
    assert_eq!(change.journal, rdir(&cfg).join("days").join(format!("{DAY}.md")));
    let bodies = client.bodies.borrow();
    let (method, body) = &bodies[0];
    assert!(method == "reconcile" && bodies.len() == 1); // ONE model call
    assert!(body.contains("## Records now") && body.contains("Loves pasta") && body.contains("## The profile page now"));
    assert!(body.contains("- My name is Sam (2026-09-11)") && body.contains("ramen for lunch now"));
    let recs = load_records(&cfg);
    assert!(recs["dining"].facts[0].starts_with("Now prefers ramen") && recs["dining"].updated == DAY);
    assert!(recs["cafe-nero"] == cafe() && recs.contains_key("ramen-bar"));
    let prof = read(rdir(&cfg).join("profile.md"));
    assert!(prof.contains("updated: 2026-09-20") && prof.contains("Order lunch without asking"));
    assert!(prof.contains("- dining (preference): food, lunch, ramen") && prof.contains("- cafe-nero (place)"));
    let journal = read(&change.journal);
    assert!(journal.contains("title: Ramen replaces pasta") && journal.contains("## What happened\n- 12:01 lunch talk"));
    assert!(journal.contains("## Still open\n- Find the ramen bar") && journal.contains("## Corrections\n- Lunch: pasta became ramen"));
    // git: the root is the records folder, never the memory root that holds the transcripts
    assert!(rdir(&cfg).join(".git").is_dir() && !cfg.store.join(".git").exists());
    let log = lines(&rgit(&cfg, &["log", "--format=%s"]));
    assert_eq!(log, vec![format!("dream: {DAY}"), format!("as found before dream: {DAY}")]);
    let old = rgit(&cfg, &["show", "HEAD~1:dining.md"]);
    assert!(old.contains("Loves pasta") && !read(rdir(&cfg).join("dining.md")).contains("Loves pasta"));
    assert_eq!(rgit(&cfg, &["status", "--porcelain"]), "");
}

#[test]
fn test_the_owner_asking_to_forget_a_record_removes_it() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&dining(), &cafe()]);
    let change = dream(&cfg, result_with(&[("records", json!([])), ("forget", json!(["cafe-nero", "profile", "../etc", "nobody"]))]));
    assert_eq!(change.unwrap().changed, 1);
    assert_eq!(ids(&load_records(&cfg)), vec!["dining"]);
    assert!(rdir(&cfg).join("profile.md").exists());
    assert!(rgit(&cfg, &["log", "--all", "--format=%s", "--name-only", "--", "cafe-nero.md"]).contains("cafe-nero"));
}

#[test]
fn test_a_failed_or_empty_dream_writes_nothing() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&dining()]);
    for bad in [Reply::Fail("503".into()), Reply::Text("not json".into()), Reply::Text("[\"a\", \"list\"]".into())] {
        let client = FakeClient::new(vec![bad]);
        assert!(with_env(&[], || reconcile_day(&cfg, &client, DAY, DAY_TEXT)).is_none());
    }
    assert_eq!(load_records(&cfg)["dining"], dining());
    assert!(!rdir(&cfg).join("profile.md").exists() && !rdir(&cfg).join("days").exists());
    let idle = FakeClient::default();
    assert!(reconcile_day(&cfg, &idle, DAY, "   ").is_none() && reconcile_day(&cfg, &idle, "../x", DAY_TEXT).is_none());
    assert!(idle.bodies.borrow().is_empty()); // no call for an empty day
}

#[test]
fn test_a_failed_call_is_logged_with_its_python_name() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    let logs = capture_logs(tracing::Level::WARN, || {
        let client = FakeClient::new(vec![Reply::Text("not json".into()), Reply::Text("[1]".into())]);
        with_env(&[], || {
            assert!(reconcile_day(&cfg, &client, DAY, DAY_TEXT).is_none());
            assert!(reconcile_day(&cfg, &client, DAY, DAY_TEXT).is_none());
        });
    });
    assert!(logs.contains("records: the reconcile call failed (JSONDecodeError)"), "{logs}");
    assert!(logs.contains("records: the reconcile call failed (ValueError)"), "{logs}");
}

#[test]
fn test_apply_skips_malformed_entries_and_never_escapes_the_folder() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    let raw = json!({"records": [
        {"id": "../escape", "type": "person", "aliases": [], "facts": ["x"]},
        {"id": "Profile", "type": "person", "aliases": [], "facts": ["x"]},
        {"id": "starred", "type": "person", "aliases": [], "facts": ["x"]},
        {"id": "empty", "type": "person", "aliases": [], "facts": []},
        {"id": "sam", "type": "not-a-type", "aliases": ["  ", "colleague"], "facts": [" Works at a shop. ", ""]},
        "junk"], "forget": ["../../etc/passwd"], "profile": {"life_context": []}, "journal": "junk"});
    let change = apply_day(&cfg, raw.as_object().unwrap(), DAY, None).unwrap();
    assert_eq!((change.changed, change.created, change.corrections), (0, 1, 0));
    let sam = load_records(&cfg)["sam"].clone();
    assert!(sam.r#type == "preference" && sam.aliases == vec!["colleague"] && sam.facts == vec!["Works at a shop."]);
    let mut names: Vec<String> =
        std::fs::read_dir(rdir(&cfg)).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    assert_eq!(names, vec!["days", "profile.md", "sam.md"]);
    assert!(!t.path().join("escape.md").exists() && !cfg.store.join("escape.md").exists());
    assert!(read(rdir(&cfg).join("profile.md")).contains("(nothing yet)"));
    assert!(read(rdir(&cfg).join("days").join(format!("{DAY}.md"))).contains("# 2026-09-20")); // an untitled day is its date
}

#[test]
fn test_an_unchanged_record_is_not_rewritten() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&cafe()]);
    let c = cafe();
    let same = json!({"id": c.id, "type": c.r#type, "aliases": c.aliases, "facts": c.facts});
    let change = dream(&cfg, result_with(&[("records", json!([same]))])).unwrap();
    assert_eq!((change.changed, change.created), (0, 0));
    assert_eq!(load_records(&cfg)["cafe-nero"].updated, "");
}

#[test]
fn test_without_git_records_are_still_written() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&dining()]);
    let seams = Seams { env: Some(env_with(&[])), which_git: Some(None), ..Seams::default() };
    let change = with_seams(seams, || reconcile_day(&cfg, &FakeClient::one(result()), DAY, DAY_TEXT));
    assert!(change.is_some());
    assert!(load_records(&cfg)["dining"].facts[0].starts_with("Now prefers ramen"));
    assert!(!rdir(&cfg).join(".git").exists());
}

#[test]
fn test_the_prompt_asks_for_corrections_and_the_journal() {
    assert!(RECONCILE_PROMPT.contains("dated correction") || RECONCILE_PROMPT.contains("correction and its date"));
    assert!(RECONCILE_PROMPT.contains("fold every real fact about them"));
    let mut req: Vec<&str> = RECONCILE_SCHEMA["properties"]["journal"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    req.sort();
    assert_eq!(req, vec!["corrections", "happened", "learned", "open", "plans", "title"]);
    assert!(!TYPES.contains(&"conversation")); // what happened once belongs in the journal
}

// ---- consolidate ------------------------------------------------------------------------------------------

fn sam_a() -> Record {
    Record::new("sam", "person", &["colleague"], &["Works at the shop (said 2026-09-01)."])
}

fn sam_b() -> Record {
    Record::new("sam-the-colleague", "person", &["sammy"], &["Likes tea (said 2026-09-02)."])
}

fn lunch() -> Record {
    Record::new("lunch-club", "routine", &["lunch"], &["Eats with [[sam-the-colleague]] on Fridays (said 2026-09-03)."])
}

#[test]
fn test_consolidate_merges_two_ids_rewrites_links_and_commits_once() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&sam_a(), &sam_b(), &lunch()]);
    dream(&cfg, result_with(&[("records", json!([]))]));
    let before = lines(&rgit(&cfg, &["log", "--format=%s"])).len();
    let merged = json!({"id": "sam", "type": "person", "aliases": ["colleague"],
                        "facts": ["Works at the shop (said 2026-09-01).", "Likes tea (said 2026-09-02)."]});
    let client = FakeClient::one(json!({"records": [merged], "merged": [{"id": "sam-the-colleague", "into": "sam"},
                                                                         {"id": "ghost", "into": "sam"}]}));
    assert_eq!(with_env(&[], || consolidate(&cfg, &client)), (2, 1)); // sam rewritten + lunch-club's link
    {
        let bodies = client.bodies.borrow();
        assert!(bodies[0].0 == "consolidate" && bodies[0].1.contains("sam-the-colleague"));
    }
    let recs = load_records(&cfg);
    assert_eq!(ids(&recs), vec!["lunch-club", "sam"]);
    for a in ["colleague", "sammy", "sam-the-colleague"] {
        assert!(recs["sam"].aliases.iter().any(|x| x == a), "{a}"); // the code joins aliases
    }
    assert_eq!(recs["lunch-club"].facts, vec!["Eats with [[sam]] on Fridays (said 2026-09-03)."]);
    let prof = read(rdir(&cfg).join("profile.md"));
    assert!(prof.contains("- sam (person)") && !prof.contains("sam-the-colleague (person)") && prof.contains("## Life context"));
    let log = lines(&rgit(&cfg, &["log", "--format=%s"]));
    assert!(log[0] == "dream: consolidate" && log.len() == before + 1);
}

#[test]
fn test_consolidate_with_nothing_to_change_is_no_commit() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&sam_a(), &sam_b()]);
    with_env(&[], || {
        assert!(ensure_repo(&rdir(&cfg)));
        commit(&rdir(&cfg), "start");
        assert_eq!(consolidate(&cfg, &FakeClient::one(json!({"records": [], "merged": []}))), (0, 0));
        assert_eq!(consolidate(&cfg, &FakeClient::new(vec![Reply::Fail("down".into())])), (0, 0));
    });
    assert_eq!(lines(&rgit(&cfg, &["log", "--format=%s"])), vec!["start"]);
    std::fs::create_dir_all(t.path().join("lone")).unwrap();
    let lone = cfg_at(&t.path().join("lone"));
    seed(&lone, &[&cafe()]);
    let idle = FakeClient::default();
    assert_eq!(consolidate(&lone, &idle), (0, 0));
    assert!(idle.bodies.borrow().is_empty()); // one one-fact record: nothing to merge
}

#[test]
fn test_consolidate_skips_records_too_big_for_one_call() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    let big_fact = "x".repeat(1000);
    let facts: Vec<&str> = vec![big_fact.as_str(); 130];
    seed(&cfg, &[&sam_a(), &Record::new("big", "project", &[], &facts)]);
    let idle = FakeClient::default();
    let logs = capture_logs(tracing::Level::WARN, || {
        assert_eq!(consolidate(&cfg, &idle), (0, 0));
    });
    assert!(idle.bodies.borrow().is_empty());
    assert!(logs.contains("not consolidated"), "{logs}");
}

// ---- forget --------------------------------------------------------------------------------------------------

#[test]
fn test_forget_lines_reaches_records_profile_stars_and_journals_but_never_frontmatter() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&dining(), &cafe()]);
    star(&cfg, "the cafe owner is rude", at(2026, 9, 21, 12, 0));
    dream(&cfg, result_with(&[("journal", journal_with(&[("happened", json!(["Talked about nero"]))]))]));
    let matches = |line: &str| {
        let l = eve_transcripts::py::casefold(line);
        ["nero", "cafe", "coffee place"].iter().any(|w| l.contains(w))
    };
    let head = rgit(&cfg, &["rev-parse", "HEAD"]);
    with_env(&[], || {
        let n = forget_lines(&cfg, &matches, true);
        assert!(n >= 4 && rgit(&cfg, &["rev-parse", "HEAD"]) == head && rdir(&cfg).join("cafe-nero.md").exists());
        assert_eq!(forget_lines(&cfg, &matches, false), n);
        assert!(!rdir(&cfg).join("cafe-nero.md").exists()); // no fact left: the record goes
        let dining_text = read(rdir(&cfg).join("dining.md"));
        assert!(dining_text.starts_with("---\nid: dining") && !dining_text.contains("cafe-nero"));
        assert!(dining_text.contains("Now prefers ramen"));
        for path in [rdir(&cfg).join("profile.md"), rdir(&cfg).join("starred.md"), rdir(&cfg).join("days").join(format!("{DAY}.md"))] {
            let body = read(&path);
            let folded = eve_transcripts::py::casefold(&body);
            let after = folded.splitn(3, "---").last().unwrap().to_string();
            assert!(!after.contains("nero") && !body.contains("cafe owner"), "{}", path.display());
        }
        assert!(read(rdir(&cfg).join("profile.md")).starts_with("---\nid: profile\nupdated: "));
        assert_eq!(rgit(&cfg, &["log", "-1", "--format=%s"]).trim(), format!("forget: {n} lines"));
        assert_eq!(forget_lines(&cfg, &matches, false), 0);
    });
}

#[test]
fn test_forget_keeps_frontmatter_lines_but_not_what_they_say() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&dining(), &cafe()]);
    dream(&cfg, result_with(&[("records", json!([])), ("journal", journal_with(&[("title", json!("Pasta day"))]))]));
    with_env(&[], || {
        let removed = forget_lines(&cfg, &|line: &str| line.contains("food") || line.to_lowercase().contains("pasta"), false);
        let dining_text = read(rdir(&cfg).join("dining.md"));
        assert!(dining_text.starts_with("---\nid: dining\ntype: preference\naliases: [lunch, restaurants, takeout]\n"));
        assert!(!dining_text.contains("pasta") && dining_text.contains("[[cafe-nero]]") && parse_record(&dining_text).is_some());
        let journal = read(rdir(&cfg).join("days").join(format!("{DAY}.md")));
        assert!(journal.starts_with("---\nday: ") && journal.contains("title: (forgotten)") && !journal.to_lowercase().contains("pasta"));
        assert_eq!(removed, 6); // alias, fact, profile index row, journal title, heading, correction
        assert!(forget_lines(&cfg, &|line: &str| line.contains("cafe-nero"), false) >= 2); // its own id: the record goes whole
    });
    assert!(!rdir(&cfg).join("cafe-nero.md").exists());
    assert!(!rdir(&cfg).join("dining.md").exists()); // its one fact left was the link there
}

#[test]
fn test_squash_history_leaves_one_commit_and_no_old_object() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&dining()]);
    dream(&cfg, result());
    let old_blob = rgit(&cfg, &["rev-parse", "HEAD~1:dining.md"]).trim().to_string();
    assert!(rgit(&cfg, &["cat-file", "-p", &old_blob]).contains("Loves pasta")); // positive control
    with_env(&[], || {
        assert!(squash_history(&rdir(&cfg)));
        assert!(!squash_history(&t.path().join("not-a-repo")));
    });
    assert_eq!(lines(&rgit(&cfg, &["log", "--all", "--format=%s"])), vec!["memory (history squashed by a forget)"]);
    assert!(!git_ok(&rdir(&cfg), &["cat-file", "-e", &old_blob])); // pruned, not merely unreachable
    assert_eq!(rgit(&cfg, &["status", "--porcelain"]), "");
    assert!(rgit(&cfg, &["show", "HEAD:dining.md"]).contains("Now prefers ramen"));
}

fn init_outer(dir: &Path) {
    assert!(std::process::Command::new("git").args(["init", "-q"]).arg(dir).status().unwrap().success());
}

#[test]
fn test_squash_history_never_touches_a_folder_inside_another_repository() {
    let t = tmp();
    init_outer(t.path());
    let inner = t.path().join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join("a.md"), "x\n").unwrap();
    assert!(git_ok(t.path(), &["add", "-A"]));
    assert!(git_ok(t.path(), &["-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", "commit", "-qm", "outer"]));
    assert!(!with_env(&[], || squash_history(&inner)));
    assert_eq!(git_in(t.path(), &["log", "--format=%s"]).trim(), "outer");
}

// ---- git: sealed, and only its own repository ------------------------------------------------------------

#[test]
fn test_a_records_folder_inside_another_repository_is_never_committed_to_it() {
    let t = tmp();
    init_outer(t.path());
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&dining()]);
    with_env(&[], || {
        assert!(!ensure_repo(&rdir(&cfg)));
        assert_eq!(commit(&rdir(&cfg), "x"), None);
    });
    assert!(dream(&cfg, result()).is_some()); // written, no history
    let out = std::process::Command::new("git").arg("-C").arg(t.path()).args(["log", "--oneline"]).output().unwrap();
    assert!(!out.status.success() || out.stdout.is_empty());
}

#[test]
fn test_an_ignore_everything_file_does_not_cost_the_history() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    put(&cfg, ".gitignore", "*\n");
    dream(&cfg, result());
    let tracked = rgit(&cfg, &["ls-files"]);
    assert!(tracked.contains("profile.md") && tracked.contains("dining.md") && tracked.contains(&format!("days/{DAY}.md")));
}

#[test]
fn test_the_memory_store_can_never_be_pushed_anywhere() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    let outside = t.path().join("outside.git");
    assert!(std::process::Command::new("git").args(["init", "-q", "--bare"]).arg(&outside).status().unwrap().success());
    std::fs::create_dir_all(rdir(&cfg)).unwrap();
    assert!(git_ok(&rdir(&cfg), &["init", "-q"]));
    assert!(git_ok(&rdir(&cfg), &["remote", "add", "origin", outside.to_str().unwrap()]));
    assert!(dream(&cfg, result()).is_some());
    assert_eq!(rgit(&cfg, &["remote"]), "");
    let uri = format!("file://{}", outside.display());
    for extra in [vec![], vec!["--no-verify"]] {
        for target in [outside.to_str().unwrap(), uri.as_str(), "https://example.invalid/x.git", "git@example.invalid:x.git"] {
            let mut args = vec!["push"];
            args.extend(extra.iter().copied());
            args.extend([target, "HEAD:refs/heads/main"]);
            assert!(!git_ok(&rdir(&cfg), &args), "{extra:?} {target}");
        }
    }
    assert!(!git_ok(&outside, &["rev-parse", "--verify", "-q", "refs/heads/main"]));
    with_env(&[], || ensure_repo(&rdir(&cfg)));
    let urls_out = rgit(&cfg, &["config", "--get-all", "url.local-only-never-pushed:.pushInsteadOf"]);
    let urls: Vec<&str> = urls_out.split_whitespace().collect();
    let unique: std::collections::HashSet<&&str> = urls.iter().collect();
    assert!(urls.len() == unique.len() && urls.len() == PUSH_PREFIXES.len(), "{urls:?}");
}

// ---- the client, and what is gone -------------------------------------------------------------------------

fn test_client(server: &FakeServer, model: &str, consolidation: &str, ledger_dir: &Path) -> OpenAIReconcileClient {
    let mut c = OpenAIReconcileClient::new(model, Some("k"), consolidation).unwrap();
    c.opts = c.opts.clone().base_url(server.base_url.clone()).max_retries(0);
    c.ledger = eve_spend::Ledger::at(ledger_dir);
    c
}

#[test]
fn test_the_openai_client_never_stores_and_uses_one_schema_per_call() {
    let t = tmp();
    let server = fake_openrouter("{\"ok\": true}");
    let client = test_client(&server, DEFAULT_MODEL, DEFAULT_CONSOLIDATION_MODEL, t.path());
    with_env(&[], || {
        assert_eq!(client.reconcile("day").unwrap(), "{\"ok\": true}");
        assert_eq!(client.consolidate("all").unwrap(), "{\"ok\": true}");
    });
    let calls = server.requests.lock().unwrap().clone();
    assert!(calls.iter().all(|(path, _)| path == "/responses"));
    assert_eq!(calls.iter().map(|(_, c)| c["store"].clone()).collect::<Vec<_>>(), vec![json!(false), json!(false)]);
    assert_eq!(calls[0].1["text"]["format"]["schema"], *RECONCILE_SCHEMA);
    assert_eq!(calls[1].1["text"]["format"]["schema"], *CONSOLIDATE_SCHEMA);
    assert!(calls.iter().all(|(_, c)| c["text"]["format"]["strict"] == json!(true)));
    assert!(make_client(Some(&HashMap::new()), DEFAULT_MODEL).is_none());
    let env: HashMap<String, String> = [("OPENROUTER_API_KEY".to_string(), "k".to_string())].into();
    assert!(make_client(Some(&env), DEFAULT_MODEL).is_some());
}

#[test]
fn test_records_extract_and_compact_with_independent_models() {
    for (model, consolidation, models) in [
        (DEFAULT_MODEL, DEFAULT_CONSOLIDATION_MODEL, ["openai/gpt-6-luna", "openai/gpt-6-luna"]),
        ("openai/gpt-6.1-sol", "deepseek/deepseek-v4.1-flash", ["openai/gpt-6.1-sol", "deepseek/deepseek-v4.1-flash"]),
    ] {
        let t = tmp();
        let server = fake_openrouter("{}");
        let client = test_client(&server, model, consolidation, t.path());
        with_env(&[], || {
            assert_eq!(client.reconcile("day's facts").unwrap(), "{}");
            assert_eq!(client.consolidate("existing records").unwrap(), "{}");
        });
        let sent = server.requests.lock().unwrap().clone();
        assert_eq!(sent.iter().map(|(_, r)| r["model"].as_str().unwrap().to_string()).collect::<Vec<_>>(), models);
        for ((_, request), name) in sent.iter().zip(["dream_day", "dream_consolidate"]) {
            assert_eq!(request["reasoning"], json!({"effort": "low"}));
            assert_eq!(request["store"], json!(false));
            assert_eq!(request["text"]["format"]["name"], name);
            assert_eq!(request["text"]["format"]["strict"], json!(true));
            assert_eq!(request["max_output_tokens"], json!(MAX_OUTPUT_TOKENS));
        }
        assert_eq!(sent[0].1["instructions"], json!(*RECONCILE_PROMPT));
        assert_eq!(sent[1].1["instructions"], json!(*CONSOLIDATE_PROMPT));
    }
}

#[test]
fn test_the_dream_is_metered_as_memory() {
    let t = tmp();
    let server = fake_openrouter("{}");
    let client = test_client(&server, "gpt-5.4-nano", DEFAULT_CONSOLIDATION_MODEL, t.path());
    with_env(&[], || client.reconcile("day").unwrap());
    let rows: Vec<Map<String, Value>> = std::fs::read_dir(t.path())
        .unwrap()
        .flatten()
        .flat_map(|e| {
            read(e.path()).lines().map(|l| serde_json::from_str::<Value>(l).unwrap().as_object().unwrap().clone()).collect::<Vec<_>>()
        })
        .collect();
    assert_eq!(rows.len(), 1);
    assert_eq!((rows[0]["f"].clone(), rows[0]["m"].clone()), (json!("memory"), json!("gpt-5.4-nano")));
}

#[test]
fn test_an_empty_reply_is_a_failed_call() {
    let t = tmp();
    let server = fake_openrouter("  ");
    let client = test_client(&server, DEFAULT_MODEL, DEFAULT_CONSOLIDATION_MODEL, t.path());
    let err = with_env(&[], || client.reconcile("day")).unwrap_err();
    assert_eq!(crate::py::error_name(&err), "RuntimeError");
    assert_eq!(err.to_string(), "empty reply");
}

#[test]
fn test_the_retired_machinery_is_gone() {
    let src = include_str!("records.rs");
    assert!(src.contains("pub fn reconcile_day") && src.contains("pub fn consolidate")); // positive control
    for name in ["Reconciler", "due_days", "day_notes", "reconciled_days", "STATE_FILE", "owner_stars", "make_reconciler",
                 "RecordsConfig", "fn configured", "RECORDS_DEFAULT"] {
        assert!(!src.contains(name), "{name}");
    }
    assert!(!src.contains(concat!("chat_", "memory")) && !src.contains(concat!("HIGH", "LIGHTS")) && !src.contains(".reconciled"));
    assert!(!src.contains(concat!("tok", "io::")));
}

// ---- event dates vs the transcript day ---------------------------------------------------------------

#[test]
fn test_the_dream_is_told_each_line_carries_its_real_date_and_plans_keep_when_apart_from_said() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    let journal = journal_with(&[
        ("happened", json!(["2026-09-21 00:08 planned the trip by text"])),
        ("plans", json!([{"what": "Dentist", "when": "2026-09-30 10:00", "said": "2026-09-21"},
                         {"what": "Visit the ramen bar", "when": "", "said": "2026-09-20"}])),
    ]);
    let client = FakeClient::new(vec![Reply::Json(result_with(&[("journal", journal)])), Reply::Json(json!({"records": [], "merged": []}))]);
    with_env(&[], || {
        reconcile_day(
            &cfg,
            &client,
            DAY,
            "2026-09-20 23:59 texted Owner: dentist on the 30th at 10\n2026-09-21 00:08 texted Owner: and the ramen bar some day",
        )
    });
    let body = client.bodies.borrow()[0].1.clone();
    assert!(body.contains("04:00 on 2026-09-20 to 04:00 on 2026-09-21"));
    assert!(body.contains("real date"));
    let schema = &RECONCILE_SCHEMA["properties"]["journal"];
    assert!(schema["required"].as_array().unwrap().contains(&json!("plans")));
    let mut req: Vec<String> = schema["properties"]["plans"]["items"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    req.sort();
    assert_eq!(req, vec!["said", "what", "when"]);
    assert!(RECONCILE_PROMPT.contains("when") && RECONCILE_PROMPT.contains("real date"));
    let page = read(rdir(&cfg).join("days").join(format!("{DAY}.md")));
    assert!(page.contains("- 2026-09-21 00:08 planned the trip by text"));
    assert!(page.contains(
        "## Plans\n- Dentist (when 2026-09-30 10:00; said 2026-09-21)\n- Visit the ramen bar (when: no date yet; said 2026-09-20)"
    ));
}

// ---- the record switches (test_memory_records.py) ----------------------------------------------------------

const MR_DAY_TEXT: &str =
    "2026-09-20 12:01 texted Owner: rent goes up to 1180 from next month\n2026-09-20 12:02 texted eve: Noted, 1180 from October.";
const OLD_RECORD: &str = "---\nid: home\ntype: place\naliases: [apartment, rent]\nupdated: 2026-09-01\n---\n\
- Rent is $1,050 a month (said 2026-09-01).\n\
- (until 2026-09-08) Lease ends August 31 (dated by 2026-09-08 13:01)\n\
- Owner's {curly} notes stay as written\n";
// The plain prompts as they were before the switches existed (sha256), so switching nothing changes nothing.
const PLAIN_RECONCILE_SHA: &str = "288bd17fbb0564981217a631e39d76aa759d62d4716221ff3a1c492a97d139df";
const PLAIN_CONSOLIDATE_SHA: &str = "f791010a10b0831d07d4c18638174c899f18b0379604f64228a34fc7462edec1";

fn sha(text: &str) -> String {
    Sha256::digest(text.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn day_result(records_out: Value, life: Option<Vec<&str>>) -> Value {
    json!({"records": records_out, "forget": [],
           "profile": {"life_context": life.unwrap_or(vec!["Lives in the home record."]), "acting": [], "talking": []},
           "journal": {"title": "Rent", "happened": [], "learned": [], "open": [], "plans": [], "corrections": []}})
}

fn fact(text: &str, said: &str, valid_from: &str, valid_to: &str) -> Value {
    json!({"text": text, "said": said, "valid_from": valid_from, "valid_to": valid_to, "src": ""})
}

#[test]
fn test_an_old_record_file_parses_and_renders_back_byte_for_byte() {
    let rec = parse_record(OLD_RECORD).unwrap();
    assert_eq!(render_record(&rec), OLD_RECORD);
    for line in &rec.facts {
        let f = parse_fact(line);
        assert_eq!(f, Fact::new(line.as_str())); // no meta: all text, braces of the owner's kept
        assert_eq!(&render_fact(&f), line);
    }
    let t = tmp();
    let cfg = cfg_at(t.path());
    put(&cfg, "home.md", OLD_RECORD);
    assert_eq!(render_record(&load_records(&cfg)["home"]), OLD_RECORD);
}

#[test]
fn test_an_old_record_searches_as_before_with_no_src_and_no_label() {
    let rec = parse_record(OLD_RECORD).unwrap();
    let mut recs = Records::new();
    recs.insert("home".into(), rec.clone());
    let hits = search(&recs, "rent lease", MAX_HITS, None);
    let mut got: Vec<String> = hits.iter().map(|h| h["line"].as_str().unwrap().to_string()).collect();
    let mut want = rec.facts.clone();
    got.sort();
    want.sort();
    assert_eq!(got, want);
    assert!(hits.iter().all(|h| h.as_object().unwrap().keys().cloned().collect::<Vec<_>>() == vec!["id", "line"]));
}

#[test]
fn test_with_both_switches_off_the_prompts_and_schemas_are_the_plain_ones() {
    assert_eq!(sha(&RECONCILE_PROMPT), PLAIN_RECONCILE_SHA);
    assert_eq!(sha(&CONSOLIDATE_PROMPT), PLAIN_CONSOLIDATE_SHA);
    assert_eq!(reconcile_prompt(false, false), *RECONCILE_PROMPT);
    assert_eq!(reconcile_schema(false, false), *RECONCILE_SCHEMA);
    assert_eq!(consolidate_schema(false, false), *CONSOLIDATE_SCHEMA);
    assert_eq!(RECONCILE_SCHEMA["properties"]["records"]["items"]["properties"]["facts"]["items"], json!({"type": "string"}));
}

#[test]
fn test_every_prompt_and_schema_is_the_pythons_byte_for_byte() {
    // sha256 of each combination's prompt and of json.dumps(schema), taken from the Python module
    let want = [
        (false, false, "288bd17fbb0564981217a631e39d76aa759d62d4716221ff3a1c492a97d139df",
         "f791010a10b0831d07d4c18638174c899f18b0379604f64228a34fc7462edec1",
         "eeddae00bd679d78d067c2ade3b3a4833083972afdac841aa5549a500570ccc9",
         "169167b9842ed3153998e124f5087da8fa05db70624255a1a00467dfd66cf840"),
        (false, true, "8c1205319fd37b9dda98c33df1baf1a44199dd362986338bb00bda55d9121ac8",
         "37c297bc6dede463aefacf5f5034719da811d791d55d2fd57ccdb4856b72c2fc",
         "ca72aa8199607ed93a61eb472013f28ea0e8898ec605809bf67c77e642ba007f",
         "b38c6625bc52d2e02c3433e7c33d92bdd12044179629643d0504a171f9cfb277"),
        (true, false, "3810a472402314f7948358b3c70cb7341962a7e75f00a1bb1a21e44337c219f8",
         "65f532218cc34b7973882dc14d1d269e392622c68291470fda351a9888b77279",
         "0585644d338e27c8d9a62b23d4b3f5f0ed60d46fc9eb8dc3029725f5cb3213d1",
         "3edc16cb43bc0b19ed67f8e858785a8b43448bb96eb3abfa586bdbc43810d573"),
        (true, true, "cbaabc48f15c9528a1326ffc490c70fb7a11e48e333cb3674bb3cd4a2606f417",
         "65f532218cc34b7973882dc14d1d269e392622c68291470fda351a9888b77279",
         "b755dab044083e04b0b7f11a7f240242b86aedc032d8858e7f3b661695dfe756",
         "fd10cf517202a847bf49af81164cab63b294db812d99dcbe86590b1b976114fe"),
    ];
    for (v, s, rp, cp, rs, cs) in want {
        assert_eq!(sha(&reconcile_prompt(v, s)), rp, "reconcile prompt {v} {s}");
        assert_eq!(sha(&consolidate_prompt(v, s)), cp, "consolidate prompt {v} {s}");
        assert_eq!(sha(&eve_transcripts::py::dumps(&reconcile_schema(v, s))), rs, "reconcile schema {v} {s}");
        assert_eq!(sha(&eve_transcripts::py::dumps(&consolidate_schema(v, s))), cs, "consolidate schema {v} {s}");
    }
}

#[test]
fn test_each_switch_adds_its_own_fields_to_the_fact_schema_and_prompt() {
    let fields = |v: bool, s: bool| -> Vec<String> {
        reconcile_schema(v, s)["properties"]["records"]["items"]["properties"]["facts"]["items"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x.as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(fields(true, false), vec!["text", "said", "valid_from", "valid_to"]);
    assert_eq!(fields(false, true), vec!["text", "src"]);
    assert_eq!(fields(true, true), vec!["text", "said", "valid_from", "valid_to", "src"]);
    assert!(reconcile_prompt(true, false).contains("valid_to") && !reconcile_prompt(true, false).contains("src"));
    assert!(reconcile_prompt(false, true).contains("src:") && !reconcile_prompt(false, true).contains("valid_to"));
    assert!(reconcile_prompt(true, false).contains("past tense"));
    assert!(consolidate_prompt(true, false).contains("valid_to"));
    // each combination is one fixed string: the prompt cache keeps holding it
    assert_eq!(reconcile_prompt(true, true), reconcile_prompt(true, true));
}

#[test]
fn test_the_meta_round_trips_and_only_known_keys_count() {
    let f = Fact {
        text: "Rent is $1,180 a month.".into(),
        said: DAY.into(),
        valid_from: "2026-10-01".into(),
        src: "2026-09-20 12:01".into(),
        ..Fact::default()
    };
    let line = render_fact(&f);
    assert_eq!(line, "Rent is $1,180 a month. {said: 2026-09-20; valid_from: 2026-10-01; src: 2026-09-20 12:01}");
    assert_eq!(parse_fact(&line), f);
    assert_eq!(parse_fact("Likes {jazz: and blues}"), Fact::new("Likes {jazz: and blues}"));
}

#[test]
fn test_validity_closes_the_old_fact_hides_it_from_the_profile_and_labels_it_in_search() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    put(&cfg, "home.md", "---\nid: home\ntype: place\naliases: [apartment, rent]\n---\n- Rent is $1,050 a month. {said: 2026-09-01; valid_from: 2026-09-01}\n");
    let old = fact("Rent is $1,050 a month.", "2026-09-01", "2026-09-01", DAY);
    let new = fact("Rent is $1,180 a month.", DAY, "2026-10-01", "");
    let life = vec!["Rent is $1,050 a month. {said: 2026-09-01; valid_to: 2026-09-20}", "Rent is $1,180 a month."];
    let out = day_result(json!([{"id": "home", "type": "place", "aliases": ["apartment", "rent"], "facts": [old, new]}]), Some(life));
    let change = with_env(&[(VALIDITY_ENV, "1")], || reconcile_day(&cfg, &FakeClient::one(out), DAY, MR_DAY_TEXT)).unwrap();
    assert_eq!(change.closed, 1);
    let rec = load_records(&cfg)["home"].clone();
    assert_eq!(rec.facts[0], "Rent is $1,050 a month. {said: 2026-09-01; valid_from: 2026-09-01; valid_to: 2026-09-20}");
    let page = profile(&cfg);
    assert!(page.contains("$1,180") && !page.contains("$1,050") && !page.contains('{'));
    let hits = search(&load_records(&cfg), "rent", MAX_HITS, None);
    assert!(hits[0]["line"].as_str().unwrap().starts_with("Rent is $1,180")); // on a tie, the open fact first
    assert_eq!(hits[1]["line"], "(until 2026-09-20) Rent is $1,050 a month. (said 2026-09-01)");
}

fn many(n: usize) -> Record {
    let facts: Vec<String> = (0..n).map(|i| format!("Fact number {i}. {{said: 2026-09-01}}")).collect();
    let refs: Vec<&str> = facts.iter().map(|s| s.as_str()).collect();
    Record::new("many", "preference", &["many"], &refs)
}

#[test]
fn test_the_guard_writes_a_busy_night_with_the_closures_over_the_limit_held_open() {
    let n = MAX_CLOSES_PER_NIGHT as usize;
    let t = tmp();
    let cfg = cfg_at(t.path());
    put(&cfg, "many.md", &render_record(&many(n + 2)));
    let out: Vec<Value> = (0..n + 2)
        .map(|i| fact(&format!("Fact number {i}."), "2026-09-01", "", if i < n + 2 { DAY } else { "" }))
        .collect();
    let res = day_result(json!([{"id": "many", "type": "preference", "aliases": ["many"], "facts": out}]), None);
    let change = with_env(&[(VALIDITY_ENV, "1")], || reconcile_day(&cfg, &FakeClient::one(res), DAY, MR_DAY_TEXT)).unwrap();
    assert_eq!(change.closed, n); // two over: written, not refused, so the night is never retried
    let facts: Vec<bool> = load_records(&cfg)["many"].facts.iter().map(|f| !parse_fact(f).valid_to.is_empty()).collect();
    let mut want = vec![true; n];
    want.extend([false, false]);
    assert_eq!(facts, want); // the first n close, the rest stay open
    let journal = read(rdir(&cfg).join("days").join(format!("{DAY}.md")));
    assert!(journal.contains(&format!("No longer true from {DAY}: Fact number {n}."))); // held, but written down, dated
    assert!(journal.contains(&format!("No longer true from {DAY}: Fact number {}.", n + 1)));
    assert!(!journal.contains("Fact number 0.")); // control: a real closure is no correction
}

#[test]
fn test_a_valid_to_set_ahead_counts_against_the_night() {
    let n = MAX_CLOSES_PER_NIGHT as usize;
    let t = tmp();
    let cfg = cfg_at(t.path());
    put(&cfg, "many.md", &render_record(&many(n + 1)));
    let later = "2026-12-31"; // after the night's day
    let out: Vec<Value> = (0..n + 1).map(|i| fact(&format!("Fact number {i}."), "2026-09-01", "", later)).collect();
    let res = day_result(json!([{"id": "many", "type": "preference", "aliases": ["many"], "facts": out}]), None);
    let change = with_env(&[(VALIDITY_ENV, "1")], || reconcile_day(&cfg, &FakeClient::one(res), DAY, MR_DAY_TEXT)).unwrap();
    assert_eq!(change.closed, n);
    assert_eq!(load_records(&cfg)["many"].facts.iter().filter(|f| !parse_fact(f).valid_to.is_empty()).count(), n);
}

#[test]
fn test_reconcile_and_consolidate_of_one_night_share_the_closure_budget() {
    let n = MAX_CLOSES_PER_NIGHT as usize;
    let t = tmp();
    let cfg = cfg_at(t.path());
    let a_facts: Vec<String> = (0..n).map(|i| format!("A{i}. {{said: 2026-09-01}}")).collect();
    let a_refs: Vec<&str> = a_facts.iter().map(|s| s.as_str()).collect();
    put(&cfg, "a.md", &render_record(&Record::new("a", "preference", &["a"], &a_refs)));
    put(&cfg, "b.md", &render_record(&Record::new("b", "preference", &["b"], &["B0. {said: 2026-09-01}", "B1."])));
    let close_a = day_result(
        json!([{"id": "a", "type": "preference", "aliases": ["a"],
                "facts": (0..n).map(|i| fact(&format!("A{i}."), "2026-09-01", "", DAY)).collect::<Vec<_>>()}]),
        None,
    );
    let close_b = json!({"records": [{"id": "b", "type": "preference", "aliases": ["b"],
                                      "facts": [fact("B0.", "2026-09-01", "", DAY), "B1."]}], "merged": []});
    let before_b = read(rdir(&cfg).join("b.md"));
    with_env(&[(VALIDITY_ENV, "1")], || {
        forgets_pinned(&cfg, || {
            assert_eq!(reconcile_day(&cfg, &FakeClient::one(close_a), DAY, MR_DAY_TEXT).unwrap().closed, n);
            assert_eq!(consolidate(&cfg, &FakeClient::one(close_b.clone())), (0, 0)); // the night's budget is spent
        });
        assert_eq!(read(rdir(&cfg).join("b.md")), before_b);
        assert_eq!(consolidate(&cfg, &FakeClient::one(close_b)), (1, 0)); // a new night: allowed
    });
}

#[test]
fn test_with_validity_off_a_valid_to_still_loads_and_the_profile_shows_open_lines_only() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    put(&cfg, "profile.md", "---\nid: profile\nupdated: 2026-09-20\n---\n# Page\n- Old job. {valid_to: 2026-01-01}\n- New job. {valid_from: 2026-01-01}\n- Plain line.\n");
    let page = profile(&cfg);
    assert!(!page.contains("Old job") && page.contains("- New job.") && page.contains("- Plain line."));
}

#[test]
fn test_sources_keep_a_src_that_points_at_a_real_line_and_drop_one_that_does_not() {
    let t = tmp();
    let cfg = cfg_at(t.path());
    let out = day_result(
        json!([{"id": "home", "type": "place", "aliases": ["rent"],
                "facts": [{"text": "Rent goes to 1180 (said 2026-09-20).", "src": "2026-09-20 12:01"},
                          {"text": "Landlord is kind (said 2026-09-20).", "src": "2026-09-20 23:59"},
                          {"text": "Pays by check (said 2026-09-20).", "src": "noon"}]}]),
        None,
    );
    with_env(&[(SOURCES_ENV, "1")], || {
        assert!(reconcile_day(&cfg, &FakeClient::one(out), DAY, MR_DAY_TEXT).is_some());
        let facts = load_records(&cfg)["home"].facts.clone();
        assert_eq!(
            facts,
            vec!["Rent goes to 1180 (said 2026-09-20). {src: 2026-09-20 12:01}", "Landlord is kind (said 2026-09-20).", "Pays by check (said 2026-09-20)."]
        );
        let hits = search(&load_records(&cfg), "rent 1180", MAX_HITS, None);
        assert_eq!(hits[0]["src"], "2026-09-20 12:01");
        assert_eq!(hits[0]["line"], "Rent goes to 1180 (said 2026-09-20). (source 2026-09-20 12:01)");
        // a src a record already carries survives a later night that does not repeat the line
        let later = day_result(
            json!([{"id": "home", "type": "place", "aliases": ["rent"],
                    "facts": [{"text": "Rent goes to 1180 (said 2026-09-20).", "src": "2026-09-20 12:01"},
                              {"text": "Rent paid (said 2026-09-21).", "src": "2026-09-21 09:00"}]}]),
            None,
        );
        assert!(reconcile_day(&cfg, &FakeClient::one(later), "2026-09-21", "2026-09-21 09:00 texted Owner: paid rent").is_some());
        assert!(load_records(&cfg)["home"].facts.iter().all(|f| f.contains("{src:")));
    });
}

#[test]
fn test_a_forget_during_the_dream_drops_its_write() {
    // forgets_pinned / the generation: a forget that lands while the model call is out voids that night's write
    struct ForgetsMidCall<'a>(&'a RecallConfig, Value);
    impl DreamClient for ForgetsMidCall<'_> {
        fn reconcile(&self, _body: &str) -> anyhow::Result<String> {
            forget_lines(self.0, &|ln: &str| ln.contains("nothing-matches-this"), false);
            Ok(self.1.to_string())
        }
        fn consolidate(&self, _body: &str) -> anyhow::Result<String> {
            forget_lines(self.0, &|ln: &str| ln.contains("nothing-matches-this"), false);
            Ok(json!({"records": [], "merged": [{"id": "sam-the-colleague", "into": "sam"}]}).to_string())
        }
    }
    let t = tmp();
    let cfg = cfg_at(t.path());
    seed(&cfg, &[&dining(), &sam_a(), &sam_b()]);
    with_env(&[], || {
        assert!(reconcile_day(&cfg, &ForgetsMidCall(&cfg, result()), DAY, DAY_TEXT).is_none());
        assert_eq!(load_records(&cfg)["dining"], dining());
        assert!(!rdir(&cfg).join("days").exists());
        assert_eq!(consolidate(&cfg, &ForgetsMidCall(&cfg, json!({}))), (0, 0));
        assert!(load_records(&cfg).contains_key("sam-the-colleague"));
        // control: with no forget in between, the same night writes
        assert!(reconcile_day(&cfg, &FakeClient::one(result()), DAY, DAY_TEXT).is_some());
    });
    assert_eq!(read(rdir(&cfg).join(FORGETS_FILE)), "2\n");
}

// ---- the switch defaults (test_memory_verdict.py, the records rows) -----------------------------------------------

#[test]
fn test_every_switch_default_equals_the_verdict() {
    let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/tests/fixtures/memory_eval/VERDICT.md")).unwrap();
    let section = text.split_once("## Defaults").unwrap().1;
    let row = regex::Regex::new(r"(?m)^\|\s*`?([a-z_]+)`?\s*\|\s*`?(EVE_MEMORY_[A-Z_]+)`?\s*\|\s*(ON|OFF)\s*\|").unwrap();
    let verdict: HashMap<String, (String, bool)> =
        row.captures_iter(section).map(|c| (c[1].to_string(), (c[2].to_string(), &c[3] == "ON"))).collect();
    let default_of = |var: &str| DEFAULTS.iter().find(|(k, _)| *k == var).unwrap().1;
    assert_eq!(verdict["validity"], (VALIDITY_ENV.to_string(), default_of(VALIDITY_ENV)));
    assert_eq!(verdict["sources"], (SOURCES_ENV.to_string(), default_of(SOURCES_ENV)));
}

#[test]
fn test_an_unset_switch_reads_its_default_and_a_set_one_reads_its_value() {
    for (var, default) in DEFAULTS {
        assert_eq!(switch(var, Some(&HashMap::new())), default);
        assert_eq!(switch(var, Some(&[(var.to_string(), String::new())].into())), default);
        assert!(switch(var, Some(&[(var.to_string(), "1".to_string())].into())));
        assert!(!switch(var, Some(&[(var.to_string(), "0".to_string())].into())));
        assert!(switch(var, Some(&[(var.to_string(), " On ".to_string())].into())));
    }
}

// ---- the talking section (test_small_talk.py) ------------------------------------------------------------------

#[test]
fn test_the_talking_section_survives_a_profile_longer_than_the_voice_cut() {
    let long_life: Vec<String> = (0..40).map(|i| format!("- Life fact number {i}: {}", "x".repeat(80))).collect();
    let talk = "## How they like to talk\n- Short and dry\n- Never starts with a greeting back";
    let page = format!(
        "---\nid: profile\nupdated: 2026-10-01\n---\n# What eve knows about its owner\n\n## Life context\n{}\n\n{talk}\n",
        long_life.join("\n")
    );
    let t = tmp();
    let cfg = cfg_at(t.path());
    put(&cfg, "profile.md", &page);
    assert!(!profile_for_voice(&cfg).contains("How they like to talk")); // the cut drops it: the bug
    assert_eq!(talking_section(&cfg), talk);
    hide(&cfg, "greeting back", None);
    assert_eq!(talking_section(&cfg), "## How they like to talk\n- Short and dry"); // hidden lines stay hidden
    put(&cfg, "profile.md", "# No sections yet\n");
    assert_eq!(talking_section(&cfg), "");
}

// ---- records' git guard (test_toolchain.py) ----------------------------------------------------------------------

fn spy() -> (GitRunner, Arc<Mutex<Vec<Vec<String>>>>) {
    let started: Arc<Mutex<Vec<Vec<String>>>> = Arc::default();
    let seen = started.clone();
    let run: GitRunner = Arc::new(move |argv: &[String]| {
        seen.lock().unwrap().push(argv.to_vec());
        Ok(GitOutput::default())
    });
    (run, started)
}

#[test]
fn test_off_macos_records_run_git_as_before() {
    let t = tmp();
    let (run, started) = spy();
    let mut tc = eve_toolchain::Toolchain::system();
    tc.platform = "linux".into();
    tc.xcode_glob = t.path().join("none*").to_string_lossy().into_owned();
    let seams = Seams { env: Some(env_with(&[])), toolchain: Some(tc), run: Some(run), ..Seams::default() };
    with_seams(seams, || git(t.path(), &["status"], GIT_TIMEOUT_SECS).unwrap());
    assert_eq!(*started.lock().unwrap(), vec![vec!["git".to_string(), "-C".into(), t.path().to_string_lossy().into_owned(), "status".into()]]);
}

/// A Mac without the developer tools: `which git` finds /usr/bin/git (the shim), git_usable says no, and any process
/// records would start is recorded instead of run.
fn unusable() -> (Seams, Arc<Mutex<Vec<Vec<String>>>>) {
    let (run, started) = spy();
    let seams = Seams {
        env: Some(env_with(&[])),
        which_git: Some(Some("/usr/bin/git".into())),
        git_usable: Some(false),
        run: Some(run),
        ..Seams::default()
    };
    (seams, started)
}

#[test]
fn test_records_git_raises_oserror_before_any_process() {
    let t = tmp();
    let (seams, started) = unusable();
    let err = with_seams(seams, || git(t.path(), &["status"], GIT_TIMEOUT_SECS)).unwrap_err();
    assert_eq!(err.py_name(), "OSError");
    assert!(started.lock().unwrap().is_empty());
}

#[test]
fn test_records_degrade_with_one_log_line_each() {
    let t = tmp();
    let repo = t.path().join("records");
    let (seams, started) = unusable();
    let logs = capture_logs(tracing::Level::WARN, || {
        with_seams(seams, || {
            assert!(!ensure_repo(&repo));
            assert_eq!(commit(&repo, "a dream"), None);
            assert!(!squash_history(&repo));
        });
    });
    assert!(started.lock().unwrap().is_empty() && !repo.join(".git").exists());
    let lines: Vec<&str> = logs.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 3, "{logs}");
    assert!(lines.iter().all(|l| l.contains("OSError")), "{logs}");
}

#[test]
fn test_in_eve_lite_records_start_no_git() {
    let t = tmp();
    let (run, started) = spy();
    let seams = Seams { env: Some(env_with(&[("EVE_PROFILE", "lite")])), run: Some(run), ..Seams::default() };
    with_seams(seams, || {
        assert_eq!(git(t.path(), &["status"], GIT_TIMEOUT_SECS).unwrap_err().py_name(), "OSError");
        assert_eq!(commit(t.path(), "a dream"), None);
    });
    assert!(started.lock().unwrap().is_empty());
}

#[test]
fn test_a_bad_profile_is_no_git_by_name() {
    let t = tmp();
    let seams = Seams { env: Some(env_with(&[("EVE_PROFILE", "huge")])), ..Seams::default() };
    let err = with_seams(seams, || git(t.path(), &["status"], GIT_TIMEOUT_SECS)).unwrap_err();
    assert_eq!(err.py_name(), "ProfileError");
}

#[test]
fn test_where_git_is_usable_records_keep_their_history() {
    let env = env_with(&[]);
    if !eve_toolchain::git_usable(Some(&env)).unwrap_or(false) {
        eprintln!("git is not usable on this machine");
        return;
    }
    let t = tmp();
    let repo = t.path().join("records");
    with_env(&[], || {
        assert!(ensure_repo(&repo));
        std::fs::write(repo.join("dining.md"), "- Loves pasta\n").unwrap();
        assert!(commit(&repo, "a dream").is_some());
        assert_eq!(lines(&git(&repo, &["log", "--format=%s"], GIT_TIMEOUT_SECS).unwrap().stdout), vec!["a dream"]);
    });
}

#[test]
fn test_a_git_that_hangs_is_timed_out() {
    let t = tmp();
    let script = t.path().join("bin");
    std::fs::create_dir_all(&script).unwrap();
    std::fs::write(script.join("git"), "#!/bin/sh\nexec /bin/sleep 5\n").unwrap();
    std::fs::set_permissions(script.join("git"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let mut env = env_with(&[]);
    env.insert("PATH".into(), script.to_string_lossy().into_owned());
    let seams = Seams { env: Some(env), git_usable: Some(true), ..Seams::default() };
    let started = std::time::Instant::now();
    let err = with_seams(seams, || git(t.path(), &["status"], 0.3)).unwrap_err();
    assert_eq!(err.py_name(), "TimeoutExpired");
    assert!(started.elapsed() < std::time::Duration::from_secs(3));
}
