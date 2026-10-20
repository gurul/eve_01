//! test_memory.py: the facade, the one tool set, forget across every store, and the one-time move.

use std::collections::{BTreeMap, HashSet};
use std::io;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{Duration, NaiveDate};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use eve_records::{RecallConfig, Record};
use eve_transcripts::transcripts::{match_query, Today, FORGOTTEN};

use crate::memory::{
    self as mem, enabled, forget_markdown, migrate_with, MeaningIndex, Memory, MemoryOptions, RecordsStore,
    MIGRATED_FILE,
};
use crate::testutil::*;

fn fake_build(tmp: &Path, index: Option<Arc<FakeIndex>>, clock: Option<&TestClock>) -> (Built, Arc<FakeRecords>) {
    let rec = FakeRecords::fixture();
    let b = build(
        tmp,
        index.map(|i| i as Arc<dyn MeaningIndex>),
        clock.map(TestClock::clock),
        Some(rec.clone() as Arc<dyn RecordsStore>),
        at(21, 12, 0),
    );
    (b, rec)
}

fn names(tools: &[Value]) -> Vec<String> {
    tools.iter().map(|t| t["name"].as_str().unwrap().to_string()).collect()
}

fn keys_of(v: &Value) -> HashSet<String> {
    v.as_object().unwrap().keys().cloned().collect()
}

fn texts(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|h| h["text"].as_str().unwrap_or("").to_string()).collect()
}

// ---- the switch --------------------------------------------------------------------------------

#[test]
fn test_enabled_is_off_by_default_and_on_with_memory_or_the_legacy_records_switch() {
    assert!(!enabled(no_env().as_ref()));
    assert!(!enabled(env_of(&[("EVE_MEMORY", "0")]).as_ref()));
    assert!(enabled(env_of(&[("EVE_MEMORY", "1")]).as_ref()));
    assert!(enabled(env_of(&[("EVE_MEMORY", "on")]).as_ref()));
    assert!(enabled(env_of(&[("EVE_RECORDS", "1")]).as_ref()));
}

// ---- the tools ---------------------------------------------------------------------------------

#[test]
fn test_tools_are_strict_in_a_stable_order_and_voice_adds_recent_conversation() {
    let d = tmp();
    let (b, _) = fake_build(d.path(), None, None);
    let m = &b.memory;
    let text = names(&m.tools(false));
    let voice = names(&m.tools(true));
    assert_eq!(text, ["memory_search", "memory_read", "forget_preview", "forget_apply"]);
    let mut want = text.clone();
    want.push("recent_conversation".into());
    assert_eq!(voice, want);
    let params: BTreeMap<String, Value> =
        m.tools(true).iter().map(|t| (t["name"].as_str().unwrap().to_string(), t["parameters"].clone())).collect();
    let set = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<HashSet<_>>();
    assert_eq!(keys_of(&params["memory_search"]["properties"]), set(&["query", "days_back"]));
    assert_eq!(params["memory_search"]["properties"]["days_back"]["type"], json!(["integer", "null"]));
    assert_eq!(keys_of(&params["memory_read"]["properties"]), set(&["ref"]));
    assert_eq!(keys_of(&params["forget_preview"]["properties"]), set(&["query"]));
    assert_eq!(keys_of(&params["forget_apply"]["properties"]), set(&["token"]));
    assert_eq!(params["recent_conversation"]["properties"], json!({}));
    for t in m.tools(true) {
        assert!(t["type"] == "function" && t["strict"] == json!(true));
        let p = &t["parameters"];
        assert_eq!(p["additionalProperties"], json!(false));
        let mut req: Vec<String> = p["required"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().into()).collect();
        let mut props: Vec<String> = keys_of(&p["properties"]).into_iter().collect();
        req.sort();
        props.sort();
        assert_eq!(req, props);
    }
}

#[test]
fn test_tools_are_copies_a_caller_cannot_corrupt() {
    let d = tmp();
    let (b, _) = fake_build(d.path(), None, None);
    let mut tools = b.memory.tools(false);
    tools[0]["description"] = json!("changed");
    assert_ne!(b.memory.tools(false)[0]["description"], json!("changed"));
}

#[test]
fn test_forget_tool_descriptions_require_an_explicit_ask_and_a_confirmation() {
    assert!(mem::FORGET_PREVIEW["description"].as_str().unwrap().contains("explicitly asks"));
    assert!(mem::FORGET_APPLY["description"].as_str().unwrap().contains("confirms in a new message"));
}

#[test]
fn test_every_tool_schema_is_the_pythons_byte_for_byte() {
    // sha256 of json.dumps(...) of each schema, taken from the Python memory.py
    let sha = |v: &Value| hex::encode(Sha256::digest(eve_transcripts::py::dumps(v).as_bytes()));
    let fixed = [
        (&*mem::MEMORY_SEARCH, "791eea16afa8aa4be7c53f5e90d9556076a6a2b73f3f5962e7758c6e26a625c1"),
        (&*mem::MEMORY_READ, "53e8035dc45a2d8ef141af91c2a4cd8758199898a78def58e12c120fa139f867"),
        (&*mem::FORGET_PREVIEW, "bcfbb46d2a07811718a86b3b5310bfc78baa49b44bfd6c95490b6d07fb5eb811"),
        (&*mem::FORGET_APPLY, "d5eef46019ed30ce07d466e3be2ae1b6c20b854903c41ccb2785f6b19cdf43e4"),
        (&*mem::RECENT_CONVERSATION, "4a3a78b78ef1eb6e088a621737313213c77ec6876d2cc23e95f140a1249b3671"),
    ];
    for (v, want) in fixed {
        assert_eq!(sha(v), want, "{}", v["name"]);
    }
    let shaped = [
        ((false, false, false, false), "791eea16afa8aa4be7c53f5e90d9556076a6a2b73f3f5962e7758c6e26a625c1"),
        ((false, false, true, false), "df484b49f088bd50c5c58ebaac8e57c4777d0839244d8d89fb400dee3a83f288"),
        ((false, false, true, true), "f89759a1f0d7b8a88c5d3b04f9a14cac5de12fc9a677a5bcfcb42fda4effb5b2"),
        ((false, true, false, true), "3f7b72b3a215f3c4f0592cb89ffb8907b3ba24a54d3126dd01b0cd735ade2d5b"),
        ((false, true, true, false), "920ecc8074bef10761a620697dd9457e22c6f48a0a457d2c5dbf038ef4611bc9"),
        ((false, true, true, true), "69a5757e10967912afe40fbd98c7d8d3afb89414ccdbbfdef4a1a709c23d6466"),
        ((true, false, false, false), "ee684becbe1118d04394d4764fd0748de534a03461c9d6012390f778c10819a0"),
        ((true, false, true, false), "8ef378ba4575b4045e73d315facc170219b137454b9a424812cc83a7fd4a27be"),
        ((true, false, true, true), "963dcf442ec0eaed6223afaafa23e967af678b071afb4ded60cf3552d6646fc3"),
        ((true, true, false, false), "315b90e7f5b9c084f89153826ce3506b1d1e5415402246e3f257c4e9b3c5ff3e"),
        ((true, true, true, false), "803708d5f0f7b0b535b9aac32755d5cbf2cd3da7ba42e7902b1f33fbd2bc8e72"),
        ((true, true, true, true), "a5b69429da3301798481efc88b5210aab7daa2e27a5f89958e70c0d78800aa6d"),
    ];
    for ((h, t, r, c), want) in shaped {
        assert_eq!(sha(&mem::memory_search_tool(h, t, r, c)), want, "{h} {t} {r} {c}");
    }
}

// ---- the prompt pieces -------------------------------------------------------------------------

#[test]
fn test_profile_voice_profile_and_star_go_to_records() {
    let d = tmp();
    let (b, _) = fake_build(d.path(), None, None);
    assert_eq!(b.memory.profile(), "PROFILE");
    assert_eq!(b.memory.profile_for_voice(), "VOICE PROFILE");
    assert!(b.memory.star("the spare key is under the mat").is_some());
    assert!(read(b.cfg.records_dir().join("starred.md")).starts_with("- the spare key is under the mat ("));
}

#[test]
fn test_a_broken_records_store_is_silence_not_a_crash() {
    let d = tmp();
    let (b, rec) = fake_build(d.path(), None, None);
    rec.state.lock().fail = true;
    assert!(b.memory.profile().is_empty() && b.memory.profile_for_voice().is_empty());
    assert!(b.memory.star("anything at all").is_none());
    assert!(b.memory.talking_section().is_empty());
}

#[test]
fn test_today_is_the_transcripts_today_block() {
    let d = tmp();
    let (b, _) = fake_build(d.path(), None, None);
    seed(&b.tx);
    let block = b.memory.today(4000, &Today::default());
    assert!(block.contains("book the dentist") && block.contains(SENTINEL));
    let hidden = b
        .memory
        .today(4000, &Today { exclude_conv: Some("t-1758459600000-bbbb".into()), ..Today::default() });
    assert!(!hidden.contains("book the dentist") && hidden.contains(SENTINEL)); // control: the other conv stays
    assert!(b.memory.today(4000, &Today { style: "voice".into(), ..Today::default() }).contains("(spoken)"));
}

// ---- memory_search / memory_read / recent_conversation ------------------------------------------

#[test]
fn test_memory_search_returns_three_dated_groups() {
    let d = tmp();
    let index = FakeIndex::new(vec![row("m1", &format!("Owner's cat is {SENTINEL}"), "2026-09-20", "voice")]);
    let (b, _) = fake_build(d.path(), Some(index), None);
    seed(&b.tx);
    write(
        &b.cfg.transcripts_dir().join("meetings").join("2026-09-20").join("1400-standup.md"),
        &format!("---\ntitle: standup\n---\n# Standup\n- the cat {SENTINEL} came up\n"),
        0o600,
    );
    let out = b.memory.handle_tool("memory_search", &json!({"query": SENTINEL, "days_back": null}), None, "");
    assert_eq!(out["ok"], json!(true));
    let ids: Vec<&str> = out["hits"].as_array().unwrap().iter().map(|h| h["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["pets"]);
    assert_eq!(out["recalled"], json!([format!("(2026-09-20, voice) Owner's cat is {SENTINEL}")]));
    let said: HashSet<(String, String)> = out["said"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| (h["day"].as_str().unwrap().into(), h["ch"].as_str().unwrap().into()))
        .collect();
    assert!(said.contains(&("2026-09-21".into(), "voice".into())));
    assert!(said.contains(&("2026-09-20".into(), "meeting".into())));
    let voice_hit = out["said"].as_array().unwrap().iter().find(|h| h["ch"] == "voice").unwrap();
    assert_eq!(voice_hit["time"], json!("09:00"));
    assert!(voice_hit["after"].as_str().unwrap().starts_with("eve:"));
    assert!(out.get("note").is_none());
}

#[test]
fn test_hidden_words_leave_every_search_group_and_no_store_changes() {
    let d = tmp();
    let index = FakeIndex::new(vec![
        row("m1", &format!("Owner's cat is {SENTINEL}"), "2026-09-20", "voice"),
        row("m2", &format!("{SENTINEL} likes the sofa"), "2026-09-19", "app"),
    ]);
    // the real records
    let b = build(d.path(), Some(index.clone() as Arc<dyn MeaningIndex>), None, None, at(21, 12, 0));
    let m = &b.memory;
    seed(&b.tx);
    say(&b.tx, "telegram", "t-1758459600000-bbbb", "owner", &format!("{SENTINEL} took the sofa again"), at(21, 11, 5));
    std::fs::create_dir_all(b.cfg.records_dir()).unwrap();
    let fact1 = format!("Has a cat called {SENTINEL}.");
    let fact2 = format!("{SENTINEL} sleeps on the sofa.");
    std::fs::write(
        b.cfg.records_dir().join("pets.md"),
        eve_records::render_record(&Record::new("pets", "preference", &["cat"], &[&fact1, &fact2])),
    )
    .unwrap();
    let out = m.search(SENTINEL, None);
    // positive control: each group has the line about to be hidden
    assert!(out["hits"].as_array().unwrap().len() == 2 && out["recalled"].as_array().unwrap().len() == 2);
    assert!(texts(&out["said"]).iter().any(|t| t.contains("asleep")));
    let snapshot = |cfg: &RecallConfig| -> BTreeMap<PathBuf, Vec<u8>> {
        let mut files = all_files(&cfg.transcripts_dir());
        files.push(cfg.records_dir().join("pets.md"));
        files.into_iter().map(|p| (p.clone(), std::fs::read(&p).unwrap())).collect()
    };
    let files = snapshot(&b.cfg);
    let rows = index.ids();

    assert!(m.hide("cat").is_some());
    assert_eq!(m.hidden(), ["cat"]);
    let out = m.search(SENTINEL, None);
    let lines: Vec<&str> = out["hits"].as_array().unwrap().iter().map(|h| h["line"].as_str().unwrap()).collect();
    assert_eq!(lines, [fact2.as_str()]);
    assert_eq!(out["recalled"], json!([format!("(2026-09-19, app) {SENTINEL} likes the sofa")]));
    assert_eq!(texts(&out["said"]), [format!("{SENTINEL} took the sofa again")]);
    assert!(out.get("note").is_none());
    assert_eq!(snapshot(&b.cfg), files);
    assert_eq!(index.ids(), rows);
    assert!(index.forgot.lock().is_empty());
    // read leaves the hidden fact out too
    assert_eq!(m.read("pets").unwrap()["record"].as_str().unwrap().matches(SENTINEL).count(), 1);

    assert!(m.unhide("cat"));
    assert!(m.hidden().is_empty());
    assert_eq!(m.search(SENTINEL, None)["hits"].as_array().unwrap().len(), 2);
}

#[test]
fn test_hide_on_a_broken_records_store_is_none_not_a_crash() {
    let d = tmp();
    let (b, _) = fake_build(d.path(), Some(FakeIndex::new(vec![])), None);
    seed(&b.tx);
    assert!(b.memory.hide("cat").is_none() && !b.memory.unhide("cat") && b.memory.hidden().is_empty());
    assert_eq!(b.memory.search(SENTINEL, None)["ok"], json!(true)); // FakeRecords has no hidden list
}

#[test]
fn test_memory_search_with_nothing_found_says_so_and_a_wordless_query_is_refused() {
    let d = tmp();
    let (b, _) = fake_build(d.path(), Some(FakeIndex::new(vec![])), None);
    seed(&b.tx);
    let empty = b.memory.handle_tool("memory_search", &json!({"query": "nonexistent-thing"}), None, "");
    assert!(empty["ok"] == json!(true) && empty["note"] == json!("nothing matched"));
    assert_eq!(b.memory.handle_tool("memory_search", &json!({"query": "  a  "}), None, "")["ok"], json!(false));
}

#[test]
fn test_memory_search_days_back_limits_the_said_group() {
    let d = tmp();
    let (b, _) = fake_build(d.path(), None, None);
    say(&b.tx, "voice", "v-1757844000000-cccc", "owner", "the old umbrella story", at(14, 12, 0));
    let wide = b.memory.handle_tool("memory_search", &json!({"query": "umbrella", "days_back": 30}), None, "");
    assert!(!wide["said"].as_array().unwrap().is_empty());
    let narrow = b.memory.handle_tool("memory_search", &json!({"query": "umbrella", "days_back": 2}), None, "");
    assert_eq!(narrow["said"], json!([]));
    // days_back as Python's int() reads it: a numeric string and a float count, anything else is the default
    let s = b.memory.handle_tool("memory_search", &json!({"query": "umbrella", "days_back": " 2 "}), None, "");
    assert_eq!(s["said"], json!([]));
    let f = b.memory.handle_tool("memory_search", &json!({"query": "umbrella", "days_back": 2.9}), None, "");
    assert_eq!(f["said"], json!([]));
    let junk = b.memory.handle_tool("memory_search", &json!({"query": "umbrella", "days_back": "two"}), None, "");
    assert!(!junk["said"].as_array().unwrap().is_empty());
}

#[test]
fn test_memory_search_survives_a_broken_records_store() {
    let d = tmp();
    let (b, rec) = fake_build(d.path(), None, None);
    rec.state.lock().fail = true;
    seed(&b.tx);
    let out = b.memory.handle_tool("memory_search", &json!({"query": "dentist"}), None, "");
    assert!(out["ok"] == json!(true) && out["hits"] == json!([]));
    assert!(!out["said"].as_array().unwrap().is_empty());
}

#[test]
fn test_memory_read_takes_a_day_a_time_range_a_start_or_a_record_id() {
    let d = tmp();
    let (b, _) = fake_build(d.path(), None, None);
    seed(&b.tx);
    let m = &b.memory;
    let read_ref = |r: &str| m.handle_tool("memory_read", &json!({"ref": r}), None, "");
    let day = read_ref("2026-09-21");
    let text = day["text"].as_str().unwrap();
    assert!(day["ok"] == json!(true) && text.contains(SENTINEL) && text.contains("dentist"));
    let ranged = read_ref("2026-09-21 10:30-11:30");
    assert!(ranged["text"].as_str().unwrap().contains("dentist") && !ranged["text"].as_str().unwrap().contains(SENTINEL));
    let from_start = read_ref("2026-09-21 10:00");
    assert!(from_start["text"].as_str().unwrap().contains("dentist"));
    assert!(!from_start["text"].as_str().unwrap().contains(SENTINEL));
    assert_eq!(read_ref("2026-09-01")["ok"], json!(false));
    assert_eq!(read_ref("pets"), json!({"ok": true, "record": "record pets"}));
    assert_eq!(read_ref("nope")["ok"], json!(false));
    assert_eq!(read_ref(" ")["ok"], json!(false));
    // whitespace folds as str.split() folds it
    let spaced = read_ref("  2026-09-21\t 10:30 -  11:30 ");
    assert!(spaced["text"].as_str().unwrap().contains("dentist"));
}

#[test]
fn test_recent_conversation_reads_only_the_other_channel_since_the_session_opened() {
    let d = tmp();
    let (b, _) = fake_build(d.path(), None, None);
    seed(&b.tx);
    let m = &b.memory;
    let out = m.handle_tool("recent_conversation", &json!({}), Some(at(21, 8, 0)), "voice");
    let text = out["text"].as_str().unwrap();
    assert!(text.contains("book the dentist") && !text.contains(SENTINEL));
    let later = m.handle_tool("recent_conversation", &json!({}), Some(at(21, 11, 0)), "voice");
    let text = later["text"].as_str().unwrap();
    assert!(!text.contains("dentist") && text.contains("Booked for Friday"));
    assert_eq!(m.handle_tool("recent_conversation", &json!({}), None, "voice")["ok"], json!(false));
    let nothing = m.handle_tool("recent_conversation", &json!({}), Some(at(21, 11, 30)), "voice");
    assert!(nothing["ok"] == json!(true) && nothing["text"] == json!(""));
}

#[test]
fn test_recent_conversation_keeps_the_newest_within_its_budget() {
    let d = tmp();
    let rec = FakeRecords::fixture();
    let b = build(d.path(), None, None, Some(rec as Arc<dyn RecordsStore>), at(21, 23, 0));
    for i in 0..80 {
        say(
            &b.tx,
            "telegram",
            "t-1758459600000-bbbb",
            "owner",
            &format!("line {i:03} {}", "x".repeat(100)),
            at(21, 12, 0) + Duration::minutes(i),
        );
    }
    let out = b.memory.handle_tool("recent_conversation", &json!({}), Some(at(21, 8, 0)), "voice");
    let text = out["text"].as_str().unwrap();
    assert!(text.chars().count() <= mem::RECENT_CHARS && text.contains("line 079") && !text.contains("line 000"));
    assert!(out["omitted"].as_u64().unwrap() > 0);
}

#[test]
fn test_unknown_tools_and_crashing_tools_answer_instead_of_raising() {
    let d = tmp();
    let (b, rec) = fake_build(d.path(), None, None);
    assert_eq!(b.memory.handle_tool("memory_write", &json!({}), None, "")["ok"], json!(false));
    assert_eq!(
        b.memory.handle_tool("memory_write", &json!({}), None, "")["reason"],
        json!("unknown tool 'memory_write'")
    );
    rec.state.lock().search_panics = true;
    assert_eq!(
        b.memory.handle_tool("memory_search", &json!({"query": "kettle"}), None, ""),
        json!({"ok": false, "reason": "memory is unavailable right now"})
    );
    assert_eq!(b.memory.handle_tool("memory_read", &json!("not a dict"), None, "")["ok"], json!(false));
    // a records reader that fails on read is an answer too
    rec.state.lock().fail = true;
    assert_eq!(
        b.memory.handle_tool("memory_read", &json!({"ref": "pets"}), None, ""),
        json!({"ok": false, "reason": "memory is unavailable right now"})
    );
}

// ---- forget ------------------------------------------------------------------------------------

struct ForgetFixture {
    b: Built,
    rec: Arc<FakeRecords>,
    index: Arc<FakeIndex>,
    clock: TestClock,
    archive: PathBuf,
    day: PathBuf,
    meeting: PathBuf,
    archive_git: PathBuf,
}

fn forget_fixture(tmp: &Path) -> ForgetFixture {
    forget_fixture_with(tmp, |_| {})
}

fn forget_fixture_with(tmp: &Path, opts: impl FnOnce(&mut MemoryOptions)) -> ForgetFixture {
    let index = FakeIndex::new(vec![
        row("m1", &format!("cat is {SENTINEL}"), "2026-09-20", "voice"),
        row("m2", "likes tea", "2026-09-19", "telegram"),
    ]);
    let clock = TestClock::new();
    let rec = FakeRecords::fixture();
    let cfg = RecallConfig::new(tmp.join("memory"), tmp.join("notes"));
    let tx = transcripts(&cfg, at(21, 12, 0));
    let mut o = MemoryOptions {
        clock: Some(clock.clock()),
        records: Some(rec.clone() as Arc<dyn RecordsStore>),
        environ: no_env(),
        ..MemoryOptions::default()
    };
    opts(&mut o);
    let memory = Memory::with_options(cfg.clone(), tx.clone(), Some(index.clone() as Arc<dyn MeaningIndex>), o);
    let b = Built { memory, tx, cfg };
    seed(&b.tx);
    let archive = write(
        &b.cfg.archive_dir().join("sessions").join("2026-09-20").join("0900-a1.md"),
        &format!("---\ntitle: about {SENTINEL}\n---\n# Talk\n- the cat {SENTINEL}\n- tea\n"),
        0o640,
    );
    let day = write(&b.cfg.archive_dir().join("2026-09-20-cats.md"), &format!("# Day\n- {SENTINEL} again\n- other\n"), 0o600);
    let meeting = write(
        &b.cfg.transcripts_dir().join("meetings").join("2026-09-20").join("1400-standup.md"),
        &format!("# Standup\n- {SENTINEL} mentioned\n- budget\n"),
        0o600,
    );
    let archive_git =
        write(&b.cfg.archive_dir().join(".git").join("objects").join("note.md"), &format!("- {SENTINEL} in git\n"), 0o600);
    std::fs::create_dir_all(b.cfg.records_dir().join(".git")).unwrap();
    ForgetFixture { b, rec, index, clock, archive, day, meeting, archive_git }
}

fn layers(transcripts: u64, records: u64, index: u64, archive: u64, meetings: u64) -> Value {
    json!({"transcripts": transcripts, "records": records, "index": index, "archive": archive, "meetings": meetings})
}

#[test]
fn test_forget_preview_counts_every_store_and_never_returns_the_words() {
    let d = tmp();
    let f = forget_fixture(d.path());
    let out = f.b.memory.handle_tool("forget_preview", &json!({"query": SENTINEL}), None, "");
    assert_eq!(out["ok"], json!(true));
    assert_eq!(out["layers"], layers(2, 1, 1, 2, 1)); // + the answer
    assert!(out["total"] == json!(7) && out["expires_in"] == json!(600));
    assert!(!out["token"].as_str().unwrap().is_empty());
    assert_eq!(out["token"].as_str().unwrap().len(), 16); // secrets.token_urlsafe(12)
    // no chat log attached: it is named as not covered, never claimed (review, 2026-10-08)
    assert_eq!(out["not_covered"], json!(["claude-mem", "vault", "chief ledger", "telegram chat log"]));
    assert!(!eve_transcripts::py::dumps(&out).to_lowercase().contains(&SENTINEL.to_lowercase()));
    // a preview changes nothing (control: the words are still everywhere)
    assert!(read(&f.archive).contains(SENTINEL) && read(&f.meeting).contains(SENTINEL));
    assert!(f.b.tx.lines("2026-09-21").iter().any(|ln| ln["text"].as_str().unwrap().contains(SENTINEL)));
    assert_eq!(f.rec.calls(), [Call::ForgetLines(true)]);
    assert!(f.index.forgot.lock().is_empty());
}

#[test]
fn test_forget_preview_with_no_match_mints_no_token_and_a_wordless_query_is_refused() {
    let d = tmp();
    let f = forget_fixture(d.path());
    let none = f.b.memory.forget_preview("nothing-like-this-anywhere");
    assert!(none["ok"] == json!(true) && none["total"] == json!(0) && none["token"] == json!(""));
    assert_eq!(none["note"], json!("nothing in memory matches"));
    assert_eq!(f.b.memory.forget_preview(" x ")["ok"], json!(false));
}

#[test]
fn test_forget_apply_removes_the_words_from_every_store_and_squashes_history() {
    let d = tmp();
    let f = forget_fixture(d.path());
    let m = &f.b.memory;
    let token = m.forget_preview(SENTINEL)["token"].as_str().unwrap().to_string();
    let out = m.handle_tool("forget_apply", &json!({"token": token}), None, "");
    assert!(out["ok"] == json!(true) && out["total"] == json!(7));
    assert_eq!(out["layers"], layers(2, 1, 1, 2, 1));
    let mut squashed: Vec<&str> = out["history_squashed"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    squashed.sort();
    assert_eq!(squashed, ["archive", "records"]);
    let calls = f.rec.calls();
    assert!(calls.contains(&Call::Squash(f.b.cfg.records_dir())) && calls.contains(&Call::Squash(f.b.cfg.archive_dir())));
    // transcripts: the envelope stays, the words go
    let rows = f.b.tx.lines("2026-09-21");
    assert!(rows.len() == 4 && rows[0]["text"] == json!(FORGOTTEN));
    assert!(!read(f.b.tx.path_of("2026-09-21")).contains(SENTINEL));
    let lines = f.rec.lines();
    assert!(lines.iter().all(|ln| !ln.contains(SENTINEL)) && lines.len() == 1);
    assert_eq!(*f.index.forgot.lock(), ["m1"]);
    assert_eq!(f.index.ids(), ["m2"]);
    // markdown: matching body lines gone, other lines and the frontmatter kept, mode kept
    assert_eq!(read(&f.archive), format!("---\ntitle: about {SENTINEL}\n---\n# Talk\n- tea\n"));
    assert_eq!(std::fs::metadata(&f.archive).unwrap().permissions().mode() & 0o7777, 0o640);
    assert_eq!(read(&f.day), "# Day\n- other\n");
    assert_eq!(read(&f.meeting), "# Standup\n- budget\n");
    assert!(read(&f.archive_git).contains(SENTINEL)); // inside .git is git's to rewrite (squash), not ours
    assert_eq!(m.forget_preview(SENTINEL)["layers"]["archive"], json!(0));
}

#[test]
fn test_a_forget_token_works_once_expires_and_belongs_to_one_memory() {
    let d = tmp();
    let f = forget_fixture(d.path());
    let m = &f.b.memory;
    let token = m.forget_preview(SENTINEL)["token"].as_str().unwrap().to_string();
    let (other, _) = fake_build(&d.path().join("other"), None, None);
    assert_eq!(other.memory.forget_apply(&token)["ok"], json!(false));
    f.clock.advance(601.0);
    assert_eq!(m.forget_apply(&token)["ok"], json!(false)); // expired
    assert!(f.b.tx.lines("2026-09-21").iter().any(|ln| ln["text"].as_str().unwrap().contains(SENTINEL)));
    let fresh = m.forget_preview(SENTINEL)["token"].as_str().unwrap().to_string();
    assert_ne!(fresh, token);
    assert_eq!(m.forget_apply(&fresh)["ok"], json!(true));
    assert_eq!(m.forget_apply(&fresh)["ok"], json!(false)); // single use
    assert_eq!(m.forget_apply("")["ok"], json!(false));
}

#[test]
fn test_at_most_sixteen_tokens_wait_and_the_oldest_goes_first() {
    let d = tmp();
    let f = forget_fixture(d.path());
    let m = &f.b.memory;
    let tokens: Vec<String> =
        (0..mem::MAX_PENDING_TOKENS + 1).map(|_| m.forget_preview(SENTINEL)["token"].as_str().unwrap().to_string()).collect();
    assert_eq!(m.forget_apply(&tokens[0])["reason"], json!("the token is unknown, used or expired; preview again"));
    assert_eq!(m.forget_apply(&tokens[mem::MAX_PENDING_TOKENS])["ok"], json!(true));
}

#[test]
fn test_forget_apply_does_not_squash_when_nothing_was_forgotten() {
    let d = tmp();
    let f = forget_fixture(d.path());
    let token = f.b.memory.forget_preview(SENTINEL)["token"].as_str().unwrap().to_string();
    // everything the preview counted vanished before the confirmation
    for p in [&f.archive, &f.day, &f.meeting] {
        std::fs::remove_file(p).unwrap();
    }
    f.b.tx.redact(SENTINEL, false);
    f.rec.state.lock().lines.clear();
    f.index.rows.lock().clear();
    let out = f.b.memory.forget_apply(&token);
    assert!(out["ok"] == json!(true) && out["total"] == json!(0) && out["history_squashed"] == json!([]));
}

#[test]
fn test_forget_apply_is_not_ok_when_mem0_could_not_be_opened() {
    // mem0 in its retry window (or with no key) answered a forget with 0, and the index read as done.
    let d = tmp();
    let index = Arc::new(FakeIndex { broken: true, ..FakeIndex::default() });
    let (b, _) = fake_build(d.path(), Some(index), None);
    let token = b.memory.forget_preview(SENTINEL)["token"].as_str().unwrap().to_string();
    let out = b.memory.forget_apply(&token);
    assert!(out["ok"] == json!(false) && out["failed"] == json!(["index"]));
    assert_eq!(
        out["reason"],
        json!("the forget did not finish in: index; the words may still be there. Preview and forget again.")
    );
}

#[test]
fn test_forget_apply_is_not_ok_when_a_markdown_file_could_not_be_rewritten() {
    // The matched lines were counted before the rewrite and its OSError swallowed: ok, with the words on disk.
    let d = tmp();
    let refused: Arc<parking_lot::Mutex<Option<PathBuf>>> = Arc::default();
    let r = refused.clone();
    let f = forget_fixture_with(d.path(), move |o| {
        o.rewrite = Some(Arc::new(move |path: &Path, text: &str| {
            if r.lock().as_deref() == Some(path) {
                return Err(io::Error::from_raw_os_error(libc::EACCES)); // PermissionError("read-only")
            }
            mem::atomic_rewrite(path, text)
        }));
    });
    *refused.lock() = Some(f.meeting.clone());
    let token = f.b.memory.forget_preview(SENTINEL)["token"].as_str().unwrap().to_string();
    let out = f.b.memory.forget_apply(&token);
    assert!(out["ok"] == json!(false) && out["failed"] == json!(["meetings"]));
    // only what really went is counted
    assert!(out["layers"]["meetings"] == json!(0) && out["layers"]["archive"] == json!(2));
    assert!(read(&f.meeting).contains(SENTINEL) && read(&f.day) == "# Day\n- other\n");
}

#[test]
fn test_forget_markdown_never_touches_frontmatter_symlinks_or_git() {
    let d = tmp();
    let folder = d.path().join("md");
    let note = write(&folder.join("a.md"), &format!("---\nid: {SENTINEL}\n---\n{SENTINEL} body\nkeep\n"), 0o600);
    let outside = write(&d.path().join("outside.md"), &format!("{SENTINEL} outside\n"), 0o600);
    std::os::unix::fs::symlink(&outside, folder.join("link.md")).unwrap();
    write(&folder.join(".git").join("x.md"), &format!("{SENTINEL}\n"), 0o600);
    write(&folder.join("plain.txt"), &format!("{SENTINEL}\n"), 0o600);
    let pred = match_query(SENTINEL).unwrap();
    assert_eq!(forget_markdown(&folder, &*pred, true).unwrap(), 1);
    assert!(read(&note).splitn(3, "---\n").nth(2).unwrap().contains(SENTINEL)); // control: dry run changed nothing
    assert_eq!(forget_markdown(&folder, &*pred, false).unwrap(), 1);
    assert_eq!(read(&note), format!("---\nid: {SENTINEL}\n---\nkeep\n"));
    assert!(read(&outside).contains(SENTINEL) && read(folder.join("plain.txt")).contains(SENTINEL));
    assert_eq!(forget_markdown(&d.path().join("missing"), &*pred, false).unwrap(), 0);
}

#[test]
fn test_forget_markdown_reads_as_python_reads_and_keeps_every_other_byte() {
    // text mode: CRLF reads as LF (and is written back so); any line boundary Python knows splits a row
    let d = tmp();
    let folder = d.path().join("md");
    let note = write(&folder.join("a.md"), "keep one\r\nzebra here\r\nkeep\u{2028}zebra two\u{0c}last\n", 0o600);
    let pred = match_query("zebra").unwrap();
    assert_eq!(forget_markdown(&folder, &*pred, false).unwrap(), 2);
    assert_eq!(read(&note), "keep one\nkeep\u{2028}last\n");
    // a frontmatter that never closes is body: forget reads every line of it
    let open = write(&folder.join("b.md"), "---\nzebra: x\nno end\n", 0o600);
    assert_eq!(forget_markdown(&folder, &*pred, false).unwrap(), 1);
    assert_eq!(read(&open), "---\nno end\n");
}

#[test]
fn test_logs_never_carry_what_was_said() {
    let d = tmp();
    let f = forget_fixture(d.path());
    let (_, logs) = capture_logs(|| {
        f.b.memory.handle_tool("memory_search", &json!({"query": SENTINEL}), None, "");
        let token = f.b.memory.forget_preview(SENTINEL)["token"].as_str().unwrap().to_string();
        f.b.memory.forget_apply(&token);
    });
    assert!(logs.contains("forgot 7 lines")); // control: the logs did run
    assert!(!logs.to_lowercase().contains(&SENTINEL.to_lowercase()));
}

#[test]
fn test_a_chat_log_is_a_layer_counted_and_forgotten() {
    // test_chat_memory.py: test_forget_removes_lines_from_the_log, on the real chat log
    let d = tmp();
    let (b, _) = fake_build(d.path(), None, None);
    let path = d.path().join("chat").join("log.json");
    let log = Arc::new(eve_chat_log::ChatLog::new(&path));
    b.memory.set_chat_log(Some(log.clone()));
    log.append(7, "user", "my locker code is a made-up zebra");
    log.append(7, "eve", "Got it.");
    let preview = b.memory.forget_preview("zebra");
    assert_eq!(preview["layers"]["chat_log"], json!(1));
    assert_eq!(preview["not_covered"], json!(["claude-mem", "vault", "chief ledger"]));
    let done = b.memory.forget_apply(preview["token"].as_str().unwrap());
    assert_eq!(done["layers"]["chat_log"], json!(1));
    let left: Vec<String> = log.lines(7).into_iter().map(|ln| ln.text).collect();
    assert_eq!(left, ["Got it."]);
    assert!(!read(&path).contains("zebra"));
    // and a reload does not bring it back
    let again: Vec<String> = eve_chat_log::ChatLog::new(&path).lines(7).into_iter().map(|ln| ln.text).collect();
    assert_eq!(again, ["Got it."]);
}

// ---- the one-time move -------------------------------------------------------------------------

const HIGHLIGHTS: &str = "# HIGHLIGHTS

> **Template.** This file is seeded with an example.

## 2026-01-01 — Example outage

- ★ Retries need an idempotency key *(example)*
- ★ Somebody else's lesson (2026-01-01)

## From talking

- ★ The dog is called Pepper (2026-09-11)
- ★ Prefers mornings for calls (2026-09-12)
- ★ (candidate) maybe likes jazz
- ★ Old example line *(example)*
";

fn old_layout(tmp: &Path) -> (PathBuf, PathBuf) {
    let legacy = tmp.join("debrief");
    let old_mem0 = tmp.join("mem0-old");
    let w = |rel: &str, text: &str| {
        write(&legacy.join(rel), text, 0o600);
    };
    w("sessions/2026-09-20/0900-a1.md", "---\nsource: eve-voice\n---\n- talk\n");
    w("sessions/2026-09-21/1000-b2.md", "- another talk\n");
    w("2026-09-20-a-day.md", "# the day\n");
    w("INDEX.md", "# INDEX\n");
    w("HIGHLIGHTS.md", HIGHLIGHTS);
    w("records/profile.md", "---\nupdated: 2026-09-20\n---\nprofile\n");
    w("records/pets.md", "---\nid: pets\n---\n- a dog\n");
    w(".git/HEAD", "ref: refs/heads/main\n");
    w(".git/objects/ab/cdef", "blob\n");
    w(".gitignore", "*\n");
    w("notes/2026-09-19/1400-standup.md", "# notes\n");
    w("notes/2026-09-19/1400-standup.transcript.md", "words\n");
    w("notes/README.md", "not a date\n");
    write(&old_mem0.join("qdrant").join("collection").join("data.bin"), "vectors\n", 0o600);
    write(&old_mem0.join("history.db"), "sqlite\n", 0o600);
    write(&old_mem0.join("ingested"), "a\n", 0o600);
    (legacy, old_mem0)
}

/// Every file os.walk lists under `root` (symlinked folders neither listed nor followed).
fn all_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(root) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        let meta = std::fs::symlink_metadata(&p).unwrap();
        if meta.is_dir() {
            out.extend(all_files(&p));
        } else if !(meta.file_type().is_symlink() && p.is_dir()) {
            out.push(p);
        }
    }
    out
}

fn digests(roots: &[&Path]) -> Vec<String> {
    let mut out: Vec<String> = roots
        .iter()
        .flat_map(|r| all_files(r))
        .filter(|p| p.file_name().unwrap() != MIGRATED_FILE)
        .map(|p| hex::encode(Sha256::digest(std::fs::read(&p).unwrap())))
        .collect();
    out.sort();
    out
}

fn file_count(roots: &[&Path]) -> usize {
    roots.iter().map(|r| all_files(r).len()).sum()
}

fn cfg_of(tmp: &Path) -> RecallConfig {
    RecallConfig::new(tmp.join("memory"), tmp.join("notes"))
}

fn rename_ok(a: &Path, b: &Path) -> io::Result<()> {
    std::fs::rename(a, b)
}

fn migrate_fake(cfg: &RecallConfig, legacy: &Path, old_mem0: &Path, rec: &FakeRecords) -> Value {
    migrate_with(cfg, legacy, old_mem0, rec, &rename_ok).unwrap()
}

#[test]
fn test_migrate_moves_the_old_layout_into_the_new_root_and_loses_nothing() {
    let d = tmp();
    let rec = FakeRecords::fixture();
    let (legacy, old_mem0) = old_layout(d.path());
    let cfg = cfg_of(d.path());
    let before_count = file_count(&[&legacy, &old_mem0]);
    let before = digests(&[&legacy, &old_mem0]);
    assert_eq!(before_count, 16); // control: the fixture is what we think

    let out = migrate_fake(&cfg, &legacy, &old_mem0, &rec);

    assert!(out["ok"] == json!(true) && out["already"] == json!(false));
    assert!(out["records"] == json!(2) && out["mem0"] == json!(3) && out["meetings"] == json!(2));
    assert_eq!(out["stars"], json!(2));
    assert!(out["archive"] == json!(9) && out["skipped"] == json!(0) && out["failed"] == json!(0));
    // every original file is somewhere under the new root, and the only new file is starred.md
    let after_count = file_count(&[&legacy, &old_mem0, &cfg.store]) - 1; // minus .migrated
    assert_eq!(after_count, before_count + 1);
    let mut after = digests(&[&legacy, &old_mem0, &cfg.store]);
    for dg in &before {
        let i = after.iter().position(|a| a == dg).expect("an original file is under the new root");
        after.remove(i);
    }
    assert_eq!(after.len(), 1); // starred.md
    assert!(file_count(&[&legacy, &old_mem0]) == 0 && legacy.is_dir()); // emptied, never deleted
    // where things went
    assert_eq!(read(cfg.records_dir().join("pets.md")), "---\nid: pets\n---\n- a dog\n");
    assert!(cfg.mem0_dir().join("history.db").exists());
    assert!(cfg.mem0_dir().join("qdrant").join("collection").join("data.bin").exists());
    assert!(cfg.transcripts_dir().join("meetings").join("2026-09-19").join("1400-standup.md").exists());
    for rel in [
        "sessions/2026-09-20/0900-a1.md",
        "2026-09-20-a-day.md",
        "INDEX.md",
        "HIGHLIGHTS.md",
        ".git/HEAD",
        ".gitignore",
        "notes/README.md",
    ] {
        assert!(cfg.archive_dir().join(rel).exists(), "{rel}");
    }
    // only the owner's own stars, dated, never the template's or a candidate
    assert_eq!(
        read(cfg.records_dir().join("starred.md")),
        "- The dog is called Pepper (2026-09-11)\n- Prefers mornings for calls (2026-09-12)\n"
    );
    assert!(rec.calls().contains(&Call::Seal(cfg.archive_dir())));
    assert_eq!(std::fs::metadata(&cfg.store).unwrap().permissions().mode() & 0o7777, 0o700);
    let marker: Value = serde_json::from_str(&read(cfg.store.join(MIGRATED_FILE))).unwrap();
    assert!(marker["records"] == json!(2) && marker["archive"] == json!(9) && marker.get("at").is_some());
    let raw = read(cfg.store.join(MIGRATED_FILE));
    assert!(raw.starts_with("{\"records\": 2, \"stars\": 2, \"mem0\": 3, ") && raw.ends_with("\"}\n"));
}

#[test]
fn test_migrate_is_idempotent() {
    let d = tmp();
    let rec = FakeRecords::fixture();
    let (legacy, old_mem0) = old_layout(d.path());
    let cfg = cfg_of(d.path());
    migrate_fake(&cfg, &legacy, &old_mem0, &rec);
    let snapshot = digests(&[&cfg.store]);
    write(&legacy.join("INDEX.md"), "# came back\n", 0o600); // something new appears in the old place
    let again = migrate_fake(&cfg, &legacy, &old_mem0, &rec);
    assert!(again["already"] == json!(true) && again["records"] == json!(2));
    assert_eq!(again["ok"], json!(true));
    assert!(digests(&[&cfg.store]) == snapshot && legacy.join("INDEX.md").exists());
}

#[test]
fn test_migrate_skips_an_existing_target_and_keeps_both() {
    let d = tmp();
    let rec = FakeRecords::fixture();
    let (legacy, old_mem0) = old_layout(d.path());
    let cfg = cfg_of(d.path());
    write(&cfg.mem0_dir().join("history.db"), "the new one\n", 0o600);
    write(&cfg.archive_dir().join("INDEX.md"), "already archived\n", 0o600);
    let before = file_count(&[&legacy, &old_mem0, &cfg.store]);
    let (out, logs) = capture_logs(|| migrate_fake(&cfg, &legacy, &old_mem0, &rec));
    assert!(out["skipped"] == json!(2) && out["mem0"] == json!(0) && out["ok"] == json!(true));
    assert_eq!(read(cfg.mem0_dir().join("history.db")), "the new one\n");
    assert_eq!(read(old_mem0.join("history.db")), "sqlite\n"); // left where it was, not lost
    assert_eq!(read(legacy.join("INDEX.md")), "# INDEX\n");
    assert_eq!(read(cfg.archive_dir().join("INDEX.md")), "already archived\n");
    assert_eq!(file_count(&[&legacy, &old_mem0, &cfg.store]) - 1, before + 1); // minus .migrated, plus starred.md
    assert!(logs.contains("skipped") && !logs.contains("INDEX") && !logs.contains("Pepper"));
}

#[test]
fn test_migrate_with_an_existing_records_folder_archives_the_old_one() {
    let d = tmp();
    let rec = FakeRecords::fixture();
    let (legacy, old_mem0) = old_layout(d.path());
    let cfg = cfg_of(d.path());
    write(&cfg.records_dir().join("profile.md"), "new profile\n", 0o600);
    migrate_fake(&cfg, &legacy, &old_mem0, &rec);
    assert_eq!(read(cfg.records_dir().join("profile.md")), "new profile\n");
    assert!(cfg.archive_dir().join("records").join("pets.md").exists());
}

#[test]
fn test_migrate_with_nothing_to_move_just_marks_it_done() {
    let d = tmp();
    let rec = FakeRecords::fixture();
    let cfg = cfg_of(d.path());
    let out = migrate_fake(&cfg, &d.path().join("no-debrief"), &d.path().join("no-mem0"), &rec);
    assert_eq!(out["ok"], json!(true));
    assert_eq!(["records", "mem0", "meetings", "archive", "stars"].iter().map(|k| out[*k].as_u64().unwrap()).sum::<u64>(), 0);
    assert!(cfg.store.join(MIGRATED_FILE).exists());
    assert!(rec.calls().is_empty()); // no archive repo: nothing to seal
}

#[test]
fn test_migrate_refuses_overlapping_folders() {
    let d = tmp();
    let rec = FakeRecords::fixture();
    let (legacy, old_mem0) = old_layout(d.path());
    let same = RecallConfig::new(legacy.clone(), d.path().join("notes"));
    let inside = RecallConfig::new(legacy.join("memory"), d.path().join("notes"));
    let count = file_count(&[&legacy]);
    let refused = migrate_fake(&same, &legacy, &old_mem0, &rec);
    assert_eq!(refused["ok"], json!(false));
    assert_eq!(refused["reason"], json!("the old store and the memory folder overlap"));
    assert_eq!(migrate_fake(&inside, &legacy, &old_mem0, &rec)["ok"], json!(false));
    assert!(file_count(&[&legacy]) == count && !legacy.join(MIGRATED_FILE).exists());
}

#[test]
fn test_a_failed_move_writes_no_marker_so_the_next_start_retries() {
    let d = tmp();
    let rec = FakeRecords::fixture();
    let (legacy, old_mem0) = old_layout(d.path());
    let cfg = cfg_of(d.path());
    let flaky = |src: &Path, dst: &Path| -> io::Result<()> {
        if src.file_name().is_some_and(|n| n == "sessions") {
            return Err(io::Error::from_raw_os_error(18)); // cross-device link
        }
        std::fs::rename(src, dst)
    };
    let first = migrate_with(&cfg, &legacy, &old_mem0, &*rec, &flaky).unwrap();
    assert!(first["ok"] == json!(false) && first["failed"] == json!(1));
    assert!(!cfg.store.join(MIGRATED_FILE).exists() && legacy.join("sessions").is_dir());
    let second = migrate_fake(&cfg, &legacy, &old_mem0, &rec);
    assert!(second["ok"] == json!(true) && second["archive"] == json!(2) && cfg.archive_dir().join("sessions").is_dir());
    assert!(cfg.store.join(MIGRATED_FILE).exists());
    assert_eq!(read(cfg.records_dir().join("starred.md")).matches("Pepper").count(), 1); // stars are not doubled
}

#[test]
fn test_a_star_that_fails_to_carry_keeps_highlights_in_place_for_the_retry() {
    let d = tmp();
    let rec = FakeRecords::fixture();
    let (legacy, old_mem0) = old_layout(d.path());
    let cfg = cfg_of(d.path());
    rec.state.lock().star_fails = true;
    let first = migrate_fake(&cfg, &legacy, &old_mem0, &rec);
    assert!(first["ok"] == json!(false) && first["failed"] == json!(2));
    assert!(legacy.join("HIGHLIGHTS.md").exists() && !cfg.archive_dir().join("HIGHLIGHTS.md").exists());
    assert!(cfg.archive_dir().join("INDEX.md").exists()); // control: the rest moved
    rec.state.lock().star_fails = false;
    let second = migrate_fake(&cfg, &legacy, &old_mem0, &rec);
    assert!(second["ok"] == json!(true) && second["stars"] == json!(2));
    assert!(cfg.archive_dir().join("HIGHLIGHTS.md").exists());
    assert!(read(cfg.records_dir().join("starred.md")).contains("Pepper"));
}

#[test]
fn test_a_legacy_mem0_that_already_is_the_new_one_is_left_alone() {
    let d = tmp();
    let rec = FakeRecords::fixture();
    let cfg = cfg_of(d.path());
    write(&cfg.mem0_dir().join("history.db"), "sqlite\n", 0o600);
    let out = migrate_fake(&cfg, &d.path().join("no-debrief"), &cfg.mem0_dir(), &rec);
    assert!(out["ok"] == json!(true) && out["skipped"] == json!(0) && out["mem0"] == json!(0));
    assert_eq!(read(cfg.mem0_dir().join("history.db")), "sqlite\n");
}

#[test]
fn test_owner_stars_read_only_the_owners_section_and_date_what_is_dated() {
    let stars = mem::owner_stars(HIGHLIGHTS);
    let day = |y, m, dd| NaiveDate::from_ymd_opt(y, m, dd).unwrap().and_hms_opt(0, 0, 0).unwrap();
    assert_eq!(
        stars,
        vec![
            ("The dog is called Pepper".to_string(), Some(day(2026, 9, 11))),
            ("Prefers mornings for calls".to_string(), Some(day(2026, 9, 12))),
        ]
    );
    // an impossible date is cut from the claim but dates nothing; an undated star stays undated
    let odd = mem::owner_stars("## From Talking\n* ★ Likes rain (2026-13-40)\n- ★   Plain star  \n");
    assert_eq!(odd, vec![("Likes rain".to_string(), None), ("Plain star".to_string(), None)]);
}
