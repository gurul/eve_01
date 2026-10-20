//! Port of bridge/tests/test_mem0_memory.py, plus the parts of the tests_ref files that exercise mem0_memory
//! (test_mem0_embedder, test_memory_records, test_forget_lean, test_memory, test_records, test_spend_wiring,
//! test_model_memory_roles, test_privacy_guards), and the helper-process boundary driven over its protocol.

use std::collections::HashMap;
use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::NaiveDate;
use eve_records::{RecallConfig, RecordsReader, Seams};
use eve_transcripts::transcripts::Wall;
use eve_transcripts::{Append, ConvInfo, Dt, Line, TranscriptConfig, Transcripts, OUTSIDE_LEFT_OUT};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use tempfile::TempDir;

use crate::mem0_memory::*;
use crate::python::{record_spend, PythonMem0, HELPER};

const DAY: &str = "2026-09-20";

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

// ---- the fake mem0 ------------------------------------------------------------------------------------------------

/// mem0's Memory, in RAM: add/search/get_all/delete, recording what it was given.
#[derive(Default)]
struct FakeState {
    added: Vec<(Value, String, Value)>,
    fail_adds: usize,
    hits: Option<Vec<Value>>,
    rows: Vec<Value>,
    deleted: Vec<String>,
    get_all_fails: bool,
    search_sleep: Option<Duration>,
    /// Keep what `add` is given as rows (test_forget_lean's RowsMem0).
    keep_rows: bool,
    embeds: Vec<(String, String)>,
    /// `search` answers the rows (test_memory_records' FakeMem0).
    search_rows: bool,
}

#[derive(Clone, Default)]
struct FakeMem0(Arc<Mutex<FakeState>>);

impl FakeMem0 {
    fn with_hits(hits: Vec<Value>) -> Self {
        let f = FakeMem0::default();
        f.0.lock().hits = Some(hits);
        f
    }
    fn failing_adds(n: usize) -> Self {
        let f = FakeMem0::default();
        f.0.lock().fail_adds = n;
        f
    }
    fn set_rows(&self, rows: Vec<Value>) {
        self.0.lock().rows = rows;
    }
    fn ids(&self) -> Vec<String> {
        self.0.lock().rows.iter().map(|r| r["id"].as_str().unwrap().to_string()).collect()
    }
    fn added(&self) -> Vec<(Value, String, Value)> {
        self.0.lock().added.clone()
    }
    fn deleted(&self) -> Vec<String> {
        self.0.lock().deleted.clone()
    }
}

impl Mem0Backend for FakeMem0 {
    fn search(&mut self, _query: &str, filters: &Value, _top_k: usize) -> anyhow::Result<Value> {
        assert_eq!(filters, &json!({"user_id": "owner"}));
        let sleep = self.0.lock().search_sleep;
        if let Some(d) = sleep {
            std::thread::sleep(d);
            return Ok(json!({"results": [{"memory": "late", "score": 0.9}]}));
        }
        let st = self.0.lock();
        if st.search_rows {
            return Ok(json!({"results": st.rows}));
        }
        Ok(json!({"results": st.hits.clone().unwrap_or_default()}))
    }

    fn get_all(&mut self, filters: &Value, _top_k: usize) -> anyhow::Result<Value> {
        assert_eq!(filters, &json!({"user_id": "owner"}));
        let st = self.0.lock();
        if st.get_all_fails {
            return Err(Mem0Error::new("RuntimeError", "qdrant").into());
        }
        Ok(json!({"results": st.rows}))
    }

    fn add(&mut self, messages: &Value, user_id: &str, metadata: &Value) -> anyhow::Result<Value> {
        let mut st = self.0.lock();
        if st.fail_adds > 0 {
            st.fail_adds -= 1;
            return Err(Mem0Error::new("RuntimeError", "network").into());
        }
        st.added.push((messages.clone(), user_id.to_string(), metadata.clone()));
        if st.keep_rows {
            let n = st.rows.len() + 1;
            let text: Vec<&str> =
                messages.as_array().unwrap().iter().map(|m| m["content"].as_str().unwrap()).collect();
            let row = json!({"id": format!("m{n}"), "memory": text.join(" ")});
            st.rows.push(row);
        }
        Ok(json!({"results": []}))
    }

    fn delete(&mut self, memory_id: &str) -> anyhow::Result<Value> {
        let mut st = self.0.lock();
        if !st.rows.iter().any(|r| r["id"] == memory_id) {
            return Err(Mem0Error::new("ValueError", "not found").into());
        }
        st.rows.retain(|r| r["id"] != memory_id);
        st.deleted.push(memory_id.to_string());
        Ok(json!({"message": "ok"}))
    }

    fn embed(&mut self, text: &str, memory_action: &str) -> anyhow::Result<bool> {
        self.0.lock().embeds.push((text.to_string(), memory_action.to_string()));
        Ok(true)
    }
}

fn factory_of(fake: &FakeMem0) -> Factory {
    let fake = fake.clone();
    Arc::new(move |_c: &Mem0Config| Ok(Box::new(fake.clone()) as Box<dyn Mem0Backend>))
}

fn broken() -> Factory {
    Arc::new(|_c: &Mem0Config| Err(Mem0Error::new("ImportError", "no mem0").into()))
}

fn rc_at(tmp: &Path) -> RecallConfig {
    let store = tmp.join("memory");
    fs::create_dir_all(&store).unwrap();
    RecallConfig { store, notes: tmp.join("notes") }
}

fn index(tmp: &Path, fake: Option<FakeMem0>) -> (OwnerMemory, FakeMem0) {
    let fake = fake.unwrap_or_default();
    let mem = OwnerMemory::new(Mem0Config::new(true, rc_at(tmp).mem0_dir()), Some(factory_of(&fake)));
    (mem, fake)
}

fn mode(p: &Path) -> u32 {
    fs::metadata(p).unwrap().permissions().mode() & 0o7777
}

// ---- switches and config ------------------------------------------------------------------------------------------

#[test]
fn test_it_is_on_with_memory_and_off_with_its_own_switch() {
    let tmp = TempDir::new().unwrap();
    let rc = rc_at(tmp.path());
    assert!(!configured(&rc, Some(&env(&[]))).enabled);
    assert!(configured(&rc, Some(&env(&[("EVE_MEMORY", "1")]))).enabled);
    assert!(configured(&rc, Some(&env(&[("EVE_RECORDS", "1")]))).enabled); // the older switch
    assert!(!configured(&rc, Some(&env(&[("EVE_MEMORY", "1"), ("EVE_MEM0", "0")]))).enabled);
    assert!(!configured(&rc, Some(&env(&[("EVE_MEM0", "1")]))).enabled); // never without memory
    assert_eq!(configured(&rc, Some(&env(&[("EVE_MEMORY", "1")]))).home, rc.mem0_dir());
    assert_eq!(rc.mem0_dir(), rc.store.join("mem0"));
    for off in [" OFF ", "false", "No"] {
        assert!(!configured(&rc, Some(&env(&[("EVE_MEMORY", "1"), ("EVE_MEM0", off)]))).enabled, "{off}");
    }
}

#[test]
fn test_everything_is_stored_under_its_home_and_extraction_is_owner_facts_only() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("m");
    let conf = mem0_config(&Mem0Config::new(true, &home));
    assert_eq!(conf["vector_store"]["provider"], "qdrant");
    assert_eq!(conf["vector_store"]["config"]["path"], home.join("qdrant").to_str().unwrap());
    assert_eq!(conf["history_db_path"], home.join("history.db").to_str().unwrap());
    let vs = conf["vector_store"]["config"].as_object().unwrap();
    assert!(!vs.contains_key("host") && !vs.contains_key("url"));
    let instructions = conf["custom_instructions"].as_str().unwrap();
    assert!(instructions.contains("owner only") && instructions.contains("is not a fact about the owner"));
}

#[test]
fn test_shared_is_none_when_off_or_not_installed_and_one_instance_otherwise() {
    let tmp = TempDir::new().unwrap();
    let rc = rc_at(tmp.path());
    let on = env(&[("EVE_MEMORY", "1")]);
    assert!(shared_with(&rc, Some(&env(&[])), || true).is_none());
    assert!(shared_with(&rc, Some(&on), || false).is_none());
    let first = shared_with(&rc, Some(&on), || true).unwrap();
    let again = shared_with(&rc, Some(&on), || true).unwrap();
    assert!(Arc::ptr_eq(&first, &again) && first.home() == rc.mem0_dir());
}

// ---- search: dated, bounded, never slow ---------------------------------------------------------------------------

#[test]
fn test_search_is_dated_with_its_channel_and_drops_guesses_stars_and_repeats() {
    let tmp = TempDir::new().unwrap();
    let hits = vec![
        json!({"id": "1", "memory": "Sister lives by the sea", "score": 0.6,
               "metadata": {"day": "2026-09-20", "channel": "voice", "conv": "v-1"}}),
        json!({"id": "2", "memory": "Likes jazz", "score": 0.5, "metadata": {"note": "sessions/2026-09-12/1015-x.md"}}),
        json!({"id": "3", "memory": "Takes the train", "score": 0.4, "created_at": "2026-09-14T08:00:00+00:00"}),
        json!({"id": "4", "memory": "A guess", "score": 0.1}),
        json!({"id": "5", "memory": "★ (candidate) eve thinks they like opera", "score": 0.9}),
        json!({"id": "6", "memory": "likes jazz.", "score": 0.45}),
    ];
    let (mem, _) = index(tmp.path(), Some(FakeMem0::with_hits(hits)));
    assert_eq!(
        mem.search("where does my sister live", MAX_HITS),
        vec![
            json!({"text": "Sister lives by the sea", "day": "2026-09-20", "channel": "voice"}),
            json!({"text": "Likes jazz", "day": "2026-09-12", "channel": ""}),
            json!({"text": "Takes the train", "day": "2026-09-14", "channel": ""}),
        ]
    );
    assert_eq!(mem.search("   ", MAX_HITS), Vec::<Value>::new());
    let many: Vec<Value> = (0..9).map(|i| json!({"id": i.to_string(), "memory": format!("fact {i}"), "score": 0.9})).collect();
    let (b, _) = index(&tmp.path().join("b"), Some(FakeMem0::with_hits(many)));
    assert_eq!(b.search("x", MAX_HITS).len(), MAX_HITS);
    assert_eq!(MAX_HITS, 5);
    assert_eq!(b.search("x", 2).len(), 2);
    assert_eq!(b.search("x", 0).len(), 1); // max(1, min(limit, MAX_HITS))
}

#[test]
fn test_a_score_is_read_as_pythons_float_and_odd_rows_are_skipped() {
    let tmp = TempDir::new().unwrap();
    let hits = vec![
        json!({"id": "1", "memory": "As a string", "score": " 0.5 "}),
        json!({"id": "2", "memory": "Not a number", "score": "high"}),
        json!({"id": "3", "memory": "No score"}),
        json!({"id": "4", "memory": "True is one", "score": true}),
        json!("not a row"),
        json!({"id": "5", "memory": "   ", "score": 0.9}),
        json!({"id": "6", "memory": "Digit groups", "score": "0_5"}),
    ];
    let (mem, _) = index(tmp.path(), Some(FakeMem0::with_hits(hits)));
    let texts: Vec<String> =
        mem.search("q", MAX_HITS).iter().map(|h| h["text"].as_str().unwrap().to_string()).collect();
    assert_eq!(texts, ["As a string", "True is one", "Digit groups"]);
}

#[test]
fn test_a_slow_search_is_abandoned_after_the_timeout() {
    assert_eq!(SEARCH_TIMEOUT_SECS, 3.0);
    let tmp = TempDir::new().unwrap();
    let slow = FakeMem0::default();
    slow.0.lock().search_sleep = Some(Duration::from_secs(5));
    let (mem, _) = index(tmp.path(), Some(slow));
    mem.set_search_timeout(0.3);
    let start = Instant::now();
    assert!(mem.search("anything", MAX_HITS).is_empty());
    assert!(start.elapsed() < Duration::from_millis(1500));
    assert!(mem.search("again", MAX_HITS).is_empty()); // a second one waits its turn, then gives up
    let start = Instant::now();
    assert!(mem.search("third", MAX_HITS).is_empty() && start.elapsed() < Duration::from_millis(100)); // no third thread
}

#[test]
fn test_keyword_hits_still_return_when_the_meaning_search_times_out() {
    // test_records.py: RecordsReader keeps its keyword hits when mem0 is slow.
    let tmp = TempDir::new().unwrap();
    let slow = FakeMem0::default();
    slow.0.lock().search_sleep = Some(Duration::from_secs(2));
    let mem = OwnerMemory::new(Mem0Config::new(true, tmp.path().join("mem0")), Some(factory_of(&slow)));
    mem.set_search_timeout(0.2);
    let rc = rc_at(tmp.path());
    let reader = RecordsReader::new(rc, Some(&mem));
    let start = Instant::now();
    let out = reader.search("pasta");
    assert!(start.elapsed() < Duration::from_millis(1500));
    assert!(out.get("recalled").is_none());
}

#[test]
fn test_a_failed_open_is_retried_after_ten_minutes() {
    let tmp = TempDir::new().unwrap();
    let now = Arc::new(Mutex::new(1000.0_f64));
    let opens = Arc::new(AtomicUsize::new(0));
    let fake = FakeMem0::with_hits(vec![json!({"memory": "Likes jazz", "score": 0.9, "metadata": {"day": DAY}})]);
    let factory: Factory = {
        let (opens, fake) = (opens.clone(), fake.clone());
        Arc::new(move |_c: &Mem0Config| {
            if opens.fetch_add(1, Ordering::SeqCst) == 0 {
                return Err(Mem0Error::new("RuntimeError", "no key yet").into());
            }
            Ok(Box::new(fake.clone()) as Box<dyn Mem0Backend>)
        })
    };
    let clock: Clock = {
        let now = now.clone();
        Arc::new(move || *now.lock())
    };
    let mem = OwnerMemory::with_clock(Mem0Config::new(true, tmp.path().join("m")), Some(factory), clock);
    assert!(mem.search("jazz", MAX_HITS).is_empty() && opens.load(Ordering::SeqCst) == 1);
    *now.lock() += 599.0;
    assert!(mem.search("jazz", MAX_HITS).is_empty() && opens.load(Ordering::SeqCst) == 1); // still inside the ten minutes
    *now.lock() += 2.0;
    assert_eq!(mem.search("jazz", MAX_HITS)[0]["text"], "Likes jazz");
    assert_eq!(opens.load(Ordering::SeqCst), 2);
}

#[test]
fn test_the_home_is_private_every_time_it_opens() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("m");
    let factory: Factory = Arc::new(|cfg: &Mem0Config| {
        fs::create_dir_all(cfg.home.join("qdrant").join("collection")).unwrap();
        fs::set_permissions(cfg.home.join("qdrant"), fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(cfg.home.join("history.db"), "").unwrap();
        fs::set_permissions(cfg.home.join("history.db"), fs::Permissions::from_mode(0o644)).unwrap();
        Ok(Box::new(FakeMem0::default()) as Box<dyn Mem0Backend>)
    });
    let mem = OwnerMemory::new(Mem0Config::new(true, &home), Some(factory));
    mem.find(&|_t| true);
    assert_eq!(mode(&home), 0o700);
    assert_eq!(mode(&home.join("qdrant")), 0o700);
    assert_eq!(mode(&home.join("qdrant").join("collection")), 0o700);
    assert_eq!(mode(&home.join("history.db")), 0o600);
}

#[test]
fn test_make_private_never_follows_a_symlink() {
    let tmp = TempDir::new().unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("f"), "").unwrap();
    fs::set_permissions(outside.join("f"), fs::Permissions::from_mode(0o644)).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o755)).unwrap();
    let home = tmp.path().join("m");
    fs::create_dir_all(&home).unwrap();
    std::os::unix::fs::symlink(&outside, home.join("link")).unwrap();
    make_private(&home);
    assert_eq!(mode(&outside), 0o755);
    assert_eq!(mode(&outside.join("f")), 0o644);
    let linked_home = tmp.path().join("linked");
    std::os::unix::fs::symlink(&outside, &linked_home).unwrap();
    make_private(&linked_home);
    assert_eq!(mode(&outside), 0o755); // a home that is a symlink is left alone
}

#[test]
fn test_mem0_unavailable_is_silence() {
    let tmp = TempDir::new().unwrap();
    let mem = OwnerMemory::new(Mem0Config::new(true, tmp.path().join("m")), Some(broken()));
    let (t, _) = transcripts_with_a_day(tmp.path());
    assert!(mem.search("anything", MAX_HITS).is_empty() && mem.ingest_day(&t, DAY) == 0);
    assert!(mem.tidy() == 0 && mem.find(&|_t| true).is_empty());
    assert!(!tmp.path().join("m").join(INGESTED_CONVS).exists()); // nothing marked read
    mem.warm(); // nothing to warm, nothing raised
}

#[test]
fn test_a_forget_mem0_cannot_open_for_is_a_failure_never_a_done_zero() {
    // A forget that could not reach mem0 (the retry window, no key) returned 0, and forget_apply then reported the
    // index as done while the memories stayed in Qdrant.
    let tmp = TempDir::new().unwrap();
    let mem = OwnerMemory::new(Mem0Config::new(true, tmp.path().join("m")), Some(broken()));
    assert!(mem.forget(&["x"]).is_err());
    assert!(mem.forget_matching(&|_t| true).is_err());
    assert_eq!(mem.forget::<&str>(&[]), Ok(0)); // nothing asked: nothing to fail

    let fake = FakeMem0::default();
    fake.0.lock().get_all_fails = true;
    let listing = OwnerMemory::new(Mem0Config::new(true, tmp.path().join("l")), Some(factory_of(&fake)));
    assert_eq!(listing.forget_matching(&|_t| true), Err(Mem0ForgetError("RuntimeError".into())));
}

#[test]
fn test_warm_opens_the_store_and_embeds_once_and_never_raises() {
    // test_mem0_embedder.py: the local embedder loads in ~3 s, more than a search's cap: the daemon warms it.
    let tmp = TempDir::new().unwrap();
    let fake = FakeMem0::default();
    let om = OwnerMemory::new(Mem0Config::new(true, tmp.path().join("mem0")), Some(factory_of(&fake)));
    om.warm();
    assert_eq!(fake.0.lock().embeds, vec![("warm".to_string(), "search".to_string())]);
    OwnerMemory::new(Mem0Config::new(true, tmp.path().join("mem0")), Some(broken())).warm(); // logged, not raised
}

// ---- ingest: each conversation, natively, once --------------------------------------------------------------------

fn at(hour: u32, minute: u32, day: &str) -> Dt {
    let d = NaiveDate::parse_from_str(day, "%Y-%m-%d").unwrap();
    eve_transcripts::py::local_aware(d.and_hms_opt(hour, minute, 0).unwrap())
}

fn conv_id(ch: &str, when: Dt) -> String {
    format!("{}-{}-abcd", &ch[..1], when.timestamp_millis())
}

fn wall_at(when: Dt) -> Wall {
    Arc::new(move || when)
}

fn put(t: &Transcripts, ch: &str, conv: &str, who: &str, kind: &str, text: &str, when: Dt) {
    assert!(t.append_with(ch, conv, who, kind, text, Append { now: Some(when), ..Default::default() }));
}

fn transcripts_with_a_day(tmp: &Path) -> (Transcripts, HashMap<&'static str, String>) {
    let t = Transcripts::new(TranscriptConfig::new(true, tmp.join("tx")), None, Some(wall_at(at(23, 0, "2026-09-21"))));
    assert!(t.enabled);
    let (voice, text, silent) = (conv_id("voice", at(9, 0, DAY)), conv_id("telegram", at(14, 0, DAY)), conv_id("voice", at(18, 0, DAY)));
    put(&t, "voice", &voice, "owner", "say", "my sister lives by the sea", at(9, 0, DAY));
    put(&t, "voice", &voice, "eve", "say", "Lovely, which town?", at(9, 1, DAY));
    assert!(t.append_with("voice", &voice, "eve", "tool", "look left",
        Append { tool: "move_head".into(), now: Some(at(9, 1, DAY)), ..Default::default() }));
    put(&t, "voice", &voice, "owner", "command", "wink", at(9, 2, DAY));
    put(&t, "voice", &voice, "owner", "relay", "ask claude to run the tests", at(9, 2, DAY));
    put(&t, "voice", &voice, "claude", "say", "tests pass", at(9, 3, DAY));
    t.close("voice", &voice, Some(at(9, 4, DAY)));
    put(&t, "telegram", &text, "owner", "say", &"x".repeat(3000), at(14, 0, DAY));
    put(&t, "telegram", &text, "eve", "say", "noted", at(14, 1, DAY));
    put(&t, "voice", &silent, "eve", "say", "hello?", at(18, 0, DAY));
    let convs = HashMap::from([("voice", voice), ("telegram", text), ("silent", silent)]);
    (t, convs)
}

#[test]
fn test_a_day_goes_in_as_user_and_assistant_messages_dated_with_channel_and_once() {
    let tmp = TempDir::new().unwrap();
    let (mem, fake) = index(tmp.path(), None);
    let (t, convs) = transcripts_with_a_day(tmp.path());
    assert_eq!(mem.ingest_day(&t, DAY), 2); // the silent one is not sent
    let added = fake.added();
    let (vmsgs, vuser, vmeta) = &added[0];
    let (tmsgs, _, tmeta) = &added[1];
    assert_eq!(
        vmsgs,
        &json!([{"role": "user", "content": "my sister lives by the sea"},
                {"role": "assistant", "content": "Lovely, which town?"}])
    ); // no tool/command/relay/claude
    assert_eq!(vuser, "owner");
    assert_eq!(vmeta, &json!({"day": DAY, "channel": "voice", "conv": convs["voice"]}));
    assert_eq!(tmeta["channel"], "telegram");
    assert_eq!(tmsgs[0]["content"].as_str().unwrap().chars().count(), MESSAGE_CHARS);
    assert!(mem.ingest_day(&t, DAY) == 0 && fake.added().len() == 2); // read once
    let home = rc_at(tmp.path()).mem0_dir();
    let mut done: Vec<String> =
        fs::read_to_string(home.join(INGESTED_CONVS)).unwrap().split_whitespace().map(str::to_string).collect();
    done.sort();
    let mut want: Vec<String> = convs.values().cloned().collect();
    want.sort();
    assert_eq!(done, want);
    assert_eq!(mode(&home.join(INGESTED_CONVS)), 0o600);
}

#[test]
fn test_a_line_kept_as_outside_words_goes_in_as_a_placeholder() {
    // Round-4 review: mem0 ingested a reply written after a read (`out`) whole, so a page's words could become a
    // memory of the owner. It keeps its place, with its words left out, as the dream's day_text does.
    let tmp = TempDir::new().unwrap();
    let (mem, fake) = index(tmp.path(), None);
    let t = Transcripts::new(TranscriptConfig::new(true, tmp.path().join("tx")), None, Some(wall_at(at(23, 0, DAY))));
    let conv = conv_id("telegram", at(10, 0, DAY));
    put(&t, "telegram", &conv, "owner", "say", "what does the page say?", at(10, 0, DAY));
    assert!(t.append_with("telegram", &conv, "eve", "say",
        "The page says the owner's bank is Example Bank, account 4410.",
        Append { now: Some(at(10, 0, DAY) + chrono::Duration::seconds(1)), outside: true, ..Default::default() }));
    assert_eq!(mem.ingest_day(&t, DAY), 1);
    assert_eq!(
        fake.added()[0].0,
        json!([{"role": "user", "content": "what does the page say?"},
               {"role": "assistant", "content": OUTSIDE_LEFT_OUT}])
    );
    let lines: Vec<Line> = vec![
        json!({"who": "eve", "kind": "say", "text": "Example Bank", "out": true, "ts": ""}).as_object().unwrap().clone(),
        json!({"who": "owner", "kind": "say", "text": "ok", "ts": ""}).as_object().unwrap().clone(),
    ];
    let units = OwnerMemory::line_units(&lines);
    assert_eq!(units[0].0[0].content, OUTSIDE_LEFT_OUT);
}

#[test]
fn test_a_long_conversation_goes_in_in_chunks_of_forty() {
    let tmp = TempDir::new().unwrap();
    let (mem, fake) = index(tmp.path(), None);
    let t = Transcripts::new(TranscriptConfig::new(true, tmp.path().join("tx")), None, Some(wall_at(at(23, 0, DAY))));
    let conv = conv_id("telegram", at(10, 0, DAY));
    for i in 0..90 {
        let who = if i % 2 == 0 { "owner" } else { "eve" };
        put(&t, "telegram", &conv, who, "say", &format!("line {i}"), at(10, 0, DAY) + chrono::Duration::seconds(i));
    }
    assert_eq!(mem.ingest_day(&t, DAY), 1);
    let sizes: Vec<usize> = fake.added().iter().map(|(m, _, _)| m.as_array().unwrap().len()).collect();
    assert_eq!(sizes, [40, 40, 10]);
}

#[test]
fn test_a_conversation_that_failed_is_tried_again() {
    let tmp = TempDir::new().unwrap();
    let (mem, fake) = index(tmp.path(), Some(FakeMem0::failing_adds(1)));
    let (t, convs) = transcripts_with_a_day(tmp.path());
    assert_eq!(mem.ingest_day(&t, DAY), 1); // voice failed; telegram went in
    let convs_in: Vec<Value> = fake.added().iter().map(|(_, _, m)| m["conv"].clone()).collect();
    assert_eq!(convs_in, [json!(convs["telegram"])]);
    assert_eq!(mem.ingest_day(&t, DAY), 1);
    assert_eq!(fake.added().last().unwrap().2["conv"], json!(convs["voice"]));
}

#[test]
fn test_messages_keep_only_said_lines_of_the_owner_and_eve() {
    let lines: Vec<Line> = [
        json!({"who": "owner", "kind": "say", "text": "  spaced\n out  "}),
        json!({"who": "owner", "kind": "photo", "text": "a photo"}),
        json!({"who": "eve", "kind": "say", "text": ""}),
        json!({"who": "system", "kind": "close", "text": ""}),
        json!({"who": "eve", "kind": "say", "text": "fine", "out": 0}),
        json!({"who": 7, "kind": "say", "text": "who is a number"}),
        json!({"who": "owner", "kind": "say", "text": 42}),
    ]
    .iter()
    .map(|v| v.as_object().unwrap().clone())
    .collect();
    let got: Vec<(String, String)> =
        OwnerMemory::messages(&lines).into_iter().map(|m| (m.role, m.content)).collect();
    assert_eq!(
        got,
        [("user".into(), "spaced out".into()), ("assistant".into(), "fine".into()), ("user".into(), "42".into())]
    );
}

// ---- EVE_MEMORY_SOURCES (test_memory_records.py) -------------------------------------------------------------------

fn tx_day(tmp: &Path) -> Transcripts {
    let rc = rc_at(tmp);
    let t0 = eve_transcripts::py::local_aware(NaiveDate::from_ymd_opt(2026, 9, 20).unwrap().and_hms_opt(12, 0, 0).unwrap());
    let tx = Transcripts::new(TranscriptConfig::new(true, rc.transcripts_dir()), Some(rc.archive_dir()), Some(wall_at(t0)));
    let conv = "t-1789930000000-abcd";
    for (mm, who, text) in [(0, "owner", "I moved to Quarry Street"), (1, "eve", "Which unit?"), (2, "owner", "Unit 5"), (3, "eve", "Got it.")] {
        put(&tx, "telegram", conv, who, "say", text, t0 + chrono::Duration::minutes(mm));
    }
    tx
}

fn with_sources<R>(on: &str, f: impl FnOnce() -> R) -> R {
    let seams = Seams { env: Some(env(&[(eve_records::records::SOURCES_ENV, on)])), ..Default::default() };
    eve_records::with_seams(seams, f)
}

#[test]
fn test_with_sources_on_mem0_gets_one_add_per_owner_line_with_its_src() {
    let tmp = TempDir::new().unwrap();
    let tx = tx_day(tmp.path());
    let fake = FakeMem0::default();
    let idx = OwnerMemory::new(Mem0Config::new(true, tmp.path().join("mem0")), Some(factory_of(&fake)));
    assert_eq!(with_sources("1", || idx.ingest_day(&tx, DAY)), 1);
    let added = fake.added();
    let msgs: Vec<Value> = added.iter().map(|(m, _, _)| m.clone()).collect();
    assert_eq!(
        msgs,
        [json!([{"role": "user", "content": "I moved to Quarry Street"}]),
         json!([{"role": "assistant", "content": "Which unit?"}, {"role": "user", "content": "Unit 5"}])]
    );
    let srcs: Vec<Value> = added.iter().map(|(_, _, m)| m["src"].clone()).collect();
    assert_eq!(srcs, [json!("2026-09-20 12:00"), json!("2026-09-20 12:02")]);
    // the second unit also holds eve's line before the owner's: it points at that line too
    assert!(added[0].2.get("src_before").is_none());
    assert_eq!(added[1].2["src_before"], "2026-09-20 12:01");
}

#[test]
fn test_with_sources_off_mem0_ingest_is_one_chunk_without_src() {
    let tmp = TempDir::new().unwrap();
    let tx = tx_day(tmp.path());
    let fake = FakeMem0::default();
    let idx = OwnerMemory::new(Mem0Config::new(true, tmp.path().join("mem0")), Some(factory_of(&fake)));
    assert_eq!(with_sources("0", || idx.ingest_day(&tx, DAY)), 1);
    let added = fake.added();
    assert!(added.len() == 1 && added[0].2.get("src").is_none());
}

#[test]
fn test_a_forget_of_eves_line_reaches_a_memory_made_from_its_unit() {
    let tmp = TempDir::new().unwrap();
    let fake = FakeMem0::default();
    fake.set_rows(vec![
        json!({"id": "m1", "memory": "Lives in unit 5", "score": 0.9,
               "metadata": {"day": DAY, "src": "2026-09-20 12:02", "src_before": "2026-09-20 12:01"}}),
        json!({"id": "m2", "memory": "Likes tea", "score": 0.9, "metadata": {"day": DAY, "src": "2026-09-20 13:00"}}),
    ]);
    let idx = OwnerMemory::new(Mem0Config::new(true, tmp.path().join("mem0")), Some(factory_of(&fake)));
    assert_eq!(idx.forget_matching(&|t| t.contains("src: 2026-09-20 12:01")), Ok(1));
    assert_eq!(fake.ids(), ["m2"]); // control: the other memory stays
}

#[test]
fn test_mem0_search_shows_src_and_a_forget_reaches_a_memory_by_its_src() {
    let tmp = TempDir::new().unwrap();
    let fake = FakeMem0::default();
    fake.0.lock().search_rows = true;
    fake.set_rows(vec![
        json!({"id": "m1", "memory": "Lives on Quarry Street", "score": 0.9,
               "metadata": {"day": DAY, "channel": "telegram", "src": "2026-09-20 12:00"}}),
        json!({"id": "m2", "memory": "Likes tea", "score": 0.9, "metadata": {"day": DAY}}),
    ]);
    let idx = OwnerMemory::new(Mem0Config::new(true, tmp.path().join("mem0")), Some(factory_of(&fake)));
    let rows = idx.search("where do I live", MAX_HITS);
    assert_eq!(rows[0]["src"], "2026-09-20 12:00");
    assert!(rows[1].get("src").is_none());
    assert_eq!(idx.forget_matching(&|t| t.contains("src: 2026-09-20 12:00")), Ok(1));
    assert_eq!(fake.ids(), ["m2"]);
}

#[test]
fn test_line_units_stamp_each_line_with_its_own_wall_time() {
    let lines: Vec<Line> = [
        json!({"who": "eve", "kind": "say", "text": "hi", "ts": "2026-09-20T23:59:30+09:00"}),
        json!({"who": "eve", "kind": "say", "text": "still there?", "ts": "not a time"}),
        json!({"who": "owner", "kind": "say", "text": "yes", "ts": "2026-09-21T00:01:00"}),
        json!({"who": "owner", "kind": "say", "text": "again", "ts": null}),
    ]
    .iter()
    .map(|v| v.as_object().unwrap().clone())
    .collect();
    let units = OwnerMemory::line_units(&lines);
    assert_eq!(units.len(), 2);
    assert_eq!(units[0].0.iter().map(|m| m.content.as_str()).collect::<Vec<_>>(), ["still there?", "yes"]);
    assert_eq!((units[0].1.as_str(), units[0].2.as_str()), ("2026-09-21 00:01", ""));
    assert_eq!((units[1].0.len(), units[1].1.as_str()), (1, ""));
}

// ---- a forget between the index's read and its add (test_forget_lean.py) ------------------------------------------

struct ForgetsAfterRead<'a> {
    tx: &'a Transcripts,
    index: &'a OwnerMemory,
    forgot: Mutex<Option<Result<usize, Mem0ForgetError>>>,
}

impl Conversations for ForgetsAfterRead<'_> {
    fn conversations(&self, day: &str) -> Vec<ConvInfo> {
        self.tx.conversations(day)
    }
    fn conv_lines(&self, conv: &str) -> Vec<Arc<Line>> {
        let lines = self.tx.conv_lines(conv);
        let mut forgot = self.forgot.lock();
        if forgot.is_none() {
            *forgot = Some(self.index.forget_matching(&|t| t.contains("SENTINEL")));
        }
        lines
    }
}

#[test]
fn test_a_forget_between_the_indexs_read_and_its_add_is_not_undone() {
    let tmp = TempDir::new().unwrap();
    let fake = FakeMem0::default();
    fake.0.lock().keep_rows = true;
    let index = OwnerMemory::new(Mem0Config::new(true, tmp.path().join("memory").join("mem0")), Some(factory_of(&fake)));
    let t = Transcripts::new(TranscriptConfig::new(true, tmp.path().join("tx")), None, Some(wall_at(at(23, 0, DAY))));
    let conv = conv_id("telegram", at(10, 0, DAY));
    put(&t, "telegram", &conv, "owner", "say", "my code is SENTINEL", at(10, 0, DAY));
    let reader = ForgetsAfterRead { tx: &t, index: &index, forgot: Mutex::new(None) };
    assert_eq!(index.ingest_day(&reader, DAY), 0);
    assert_eq!(*reader.forgot.lock(), Some(Ok(0))); // the forget ran between read and add
    assert!(fake.added().is_empty());
    // control: with nothing forgotten in between, the same day does go in
    assert_eq!(index.ingest_day(&t, DAY), 1);
    assert_eq!(fake.added().len(), 1);
}

// ---- tidy, find, forget ---------------------------------------------------------------------------------------------

fn rows4() -> Vec<Value> {
    vec![
        json!({"id": "a", "memory": "Likes jazz", "created_at": "2026-09-01"}),
        json!({"id": "b", "memory": "likes  JAZZ.", "created_at": "2026-09-02"}),
        json!({"id": "c", "memory": "★ (candidate) Likes opera", "created_at": "2026-09-03"}),
        json!({"id": "d", "memory": "Sister lives by the sea", "created_at": "2026-09-04"}),
    ]
}

#[test]
fn test_tidy_drops_exact_duplicates_and_star_candidates_keeping_the_oldest() {
    let tmp = TempDir::new().unwrap();
    let (mem, fake) = index(tmp.path(), None);
    let mut rows = rows4();
    rows.swap(0, 1); // listed newest first: the oldest still stays
    fake.set_rows(rows);
    assert_eq!(mem.tidy(), 2);
    let mut deleted = fake.deleted();
    deleted.sort();
    assert_eq!(deleted, ["b", "c"]);
    assert_eq!(fake.ids(), ["a", "d"]);
    assert_eq!(mem.tidy(), 0);
}

fn history_db(path: &Path) {
    // The two tables mem0ai 2.2.0 keeps in history.db (mem0/memory/storage.py), with rows.
    let con = rusqlite::Connection::open(path).unwrap();
    con.execute_batch(
        "CREATE TABLE history (id TEXT PRIMARY KEY, memory_id TEXT, old_memory TEXT, new_memory TEXT, \
         event TEXT, created_at DATETIME, updated_at DATETIME, is_deleted INTEGER, actor_id TEXT, role TEXT);
         CREATE TABLE messages (id TEXT PRIMARY KEY, session_scope TEXT, role TEXT, content TEXT, \
         name TEXT, created_at DATETIME);
         INSERT INTO history (id, memory_id, old_memory, new_memory, event) VALUES
           ('h1', 'd', NULL, 'Sister lives by the sea', 'ADD'),
           ('h2', 'd', 'Sister lives by the sea', NULL, 'DELETE'),
           ('h3', 'a', NULL, 'Likes jazz', 'ADD');
         INSERT INTO messages VALUES ('m1', 'user_id=owner', 'user', 'my sister lives by the sea', NULL, '');",
    )
    .unwrap();
}

fn contains_lower(path: &Path, needle: &str) -> bool {
    String::from_utf8_lossy(&fs::read(path).unwrap()).to_lowercase().contains(needle)
}

#[test]
fn test_forget_deletes_the_memory_and_its_history_rows() {
    let tmp = TempDir::new().unwrap();
    let (mem, fake) = index(tmp.path(), None);
    fake.set_rows(rows4());
    let home = rc_at(tmp.path()).mem0_dir();
    fs::create_dir_all(&home).unwrap();
    history_db(&home.join("history.db"));
    assert!(contains_lower(&home.join("history.db"), "sister lives by the sea")); // positive control
    let found = mem.find(&|text| eve_transcripts::py::casefold(text).contains("sister"));
    assert_eq!(found, [Found { id: "d".into(), text: "Sister lives by the sea".into() }]);
    let mut ids: Vec<String> = found.iter().map(|f| f.id.clone()).collect();
    ids.extend(["missing".to_string(), String::new()]);
    assert_eq!(mem.forget(&ids), Ok(1));
    assert_eq!(fake.deleted(), ["d"]);
    let con = rusqlite::Connection::open(home.join("history.db")).unwrap();
    let left: Vec<String> =
        con.prepare("SELECT memory_id FROM history").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    let msgs: i64 = con.query_row("SELECT COUNT(*) FROM messages", [], |r| r.get(0)).unwrap();
    drop(con);
    assert!(left == ["a"] && msgs == 0); // other memories' rows stay
    assert!(!contains_lower(&home.join("history.db"), "sister lives by the sea"));
    assert_eq!(mem.forget::<&str>(&[]), Ok(0));
}

#[test]
fn test_forget_without_a_history_database_still_deletes() {
    let tmp = TempDir::new().unwrap();
    let (mem, fake) = index(tmp.path(), None);
    fake.set_rows(rows4());
    assert_eq!(mem.forget(&["a"]), Ok(1));
    assert_eq!(fake.deleted(), ["a"]);
}

#[test]
fn test_a_history_database_that_is_not_one_is_logged_and_the_forget_still_counts() {
    let tmp = TempDir::new().unwrap();
    let (mem, fake) = index(tmp.path(), None);
    fake.set_rows(rows4());
    let home = rc_at(tmp.path()).mem0_dir();
    fs::create_dir_all(home.join("qdrant").join("collection").join("eve")).unwrap();
    fs::write(home.join("history.db"), "not sqlite at all, but long enough to have a header of sorts......").unwrap();
    // a qdrant store's sqlite is compacted too
    let store = home.join("qdrant").join("collection").join("eve").join("storage.sqlite");
    rusqlite::Connection::open(&store).unwrap().execute_batch("CREATE TABLE points (id TEXT, point BLOB)").unwrap();
    assert_eq!(mem.forget(&["d"]), Ok(1));
    assert!(store.exists());
}

// ---- spend: mem0's two model calls are metered (test_spend_wiring.py) ----------------------------------------------

fn ledger_lines(dir: &Path) -> Vec<Map<String, Value>> {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    eve_spend::Ledger::at(dir).day_rows(&today)
}

#[test]
fn test_mem0s_extraction_and_embeddings_are_metered() {
    let tmp = TempDir::new().unwrap();
    let ledger = eve_spend::Ledger::at(tmp.path());
    let cfg = Mem0Config::default();
    let m = 1_000_000;
    record_spend(&ledger, &cfg, "chat", "gpt-5.4-nano", &json!({"usage": {"prompt_tokens": m, "completion_tokens": 0}}));
    record_spend(&ledger, &cfg, "embedding", "text-embedding-3-small",
        &json!({"usage": {"prompt_tokens": m, "total_tokens": m}}));
    let lines = ledger_lines(tmp.path());
    let (a, b) = (&lines[0], &lines[1]);
    assert_eq!((a["f"].as_str(), a["p"].as_str(), a["m"].as_str()), (Some("memory"), Some("openrouter"), Some("gpt-5.4-nano")));
    assert!((a["usd"].as_f64().unwrap() - 0.20).abs() < 1e-9);
    assert_eq!(b["m"], "text-embedding-3-small");
    assert!((b["usd"].as_f64().unwrap() - 0.02).abs() < 1e-9);
    // a call that named no model is the config's
    record_spend(&ledger, &cfg, "chat", "", &json!({}));
    assert_eq!(ledger_lines(tmp.path())[2]["m"], DEFAULT_MODEL);
}

// ---- the embedder switch (test_mem0_embedder.py, test_model_memory_roles.py) ---------------------------------------

#[test]
fn test_the_default_keeps_remote_embeddings_with_low_effort_extraction() {
    let tmp = TempDir::new().unwrap();
    let rc = rc_at(tmp.path());
    for extra in [None, Some(""), Some("openrouter"), Some("something-else")] {
        let mut e = env(&[("EVE_MEMORY", "1")]);
        if let Some(v) = extra {
            e.insert("EVE_MEM0_EMBEDDER".into(), v.into());
        }
        let cfg = configured(&rc, Some(&e));
        assert_eq!(cfg, Mem0Config::new(true, rc.mem0_dir()));
        assert!(cfg.embedder == "openrouter" && cfg.embed_model == "text-embedding-3-small");
    }
    let home = rc.mem0_dir();
    let want = json!({
        "llm": {"provider": "openai", "config": {
            "model": "gpt-6-luna", "api_key": PLACEHOLDER_KEY, "reasoning_effort": "low",
            "is_reasoning_model": true, "max_tokens": EXTRACT_MAX_OUTPUT_TOKENS}},
        "embedder": {"provider": "openai", "config": {"model": "text-embedding-3-small", "api_key": PLACEHOLDER_KEY}},
        "vector_store": {"provider": "qdrant", "config": {
            "collection_name": "eve", "path": home.join("qdrant").to_str().unwrap(), "on_disk": true,
            "embedding_model_dims": 1536}},
        "history_db_path": home.join("history.db").to_str().unwrap(),
        "custom_instructions": CUSTOM_INSTRUCTIONS,
    });
    let got = mem0_config(&configured(&rc, Some(&env(&[("EVE_MEMORY", "1")]))));
    assert_eq!(got, want);
    // and in Python's key order (json.dumps of the dict)
    assert_eq!(serde_json::to_string(&got).unwrap(), serde_json::to_string(&want).unwrap());
}

#[test]
fn test_gemma_is_a_separate_768_store_under_its_own_home_read_from_the_cache_only() {
    let tmp = TempDir::new().unwrap();
    let rc = rc_at(tmp.path());
    let cfg = configured(&rc, Some(&env(&[("EVE_MEMORY", "1"), ("EVE_MEM0_EMBEDDER", " Gemma ")])));
    assert!(cfg.enabled && cfg.embedder == "gemma" && cfg.home == rc.mem0_dir().join("gemma"));
    let conf = mem0_config(&cfg);
    let emb = &conf["embedder"];
    assert_eq!(emb["provider"], "huggingface");
    assert!(emb["config"]["model"] == "unsloth/embeddinggemma-300m" && emb["config"]["embedding_dims"] == 768);
    // On CPU: the default MPS device held 1.2 GB of GPU memory for the life of the daemon.
    assert_eq!(emb["config"]["model_kwargs"], json!({"revision": GEMMA_REVISION, "local_files_only": true, "device": "cpu"}));
    assert_eq!(GEMMA_REVISION.len(), 40); // a pinned commit, not a branch
    assert!(emb["config"].get("huggingface_base_url").is_none()); // local, not a hosted endpoint
    let vs = &conf["vector_store"]["config"];
    assert!(vs["collection_name"] == "eve_gemma" && vs["embedding_model_dims"] == 768);
    assert_eq!(vs["path"], rc.mem0_dir().join("gemma").join("qdrant").to_str().unwrap());
    assert_ne!(vs["path"], rc.mem0_dir().join("qdrant").to_str().unwrap());
    assert_eq!(conf["history_db_path"], rc.mem0_dir().join("gemma").join("history.db").to_str().unwrap());
    assert_eq!(conf["llm"], mem0_config(&configured(&rc, Some(&env(&[("EVE_MEMORY", "1")]))))["llm"]); // extraction unchanged
    assert!(!configured(&rc, Some(&env(&[("EVE_MEMORY", "1"), ("EVE_MEM0_EMBEDDER", "gemma"), ("EVE_MEM0", "0")]))).enabled);
}

#[test]
fn test_shared_gives_gemma_its_own_instance() {
    let tmp = TempDir::new().unwrap();
    let rc = rc_at(tmp.path());
    let a = shared_with(&rc, Some(&env(&[("EVE_MEMORY", "1")])), || true).unwrap();
    let b = shared_with(&rc, Some(&env(&[("EVE_MEMORY", "1"), ("EVE_MEM0_EMBEDDER", "gemma")])), || true).unwrap();
    assert!(!Arc::ptr_eq(&a, &b));
    assert!(a.home() == rc.mem0_dir() && b.home() == rc.mem0_dir().join("gemma"));
}

#[test]
fn test_local_embeddings_keep_the_same_extraction_settings() {
    let remote = mem0_config(&Mem0Config::default());
    let local = mem0_config(&Mem0Config { embedder: "gemma".into(), ..Default::default() });
    assert_eq!(local["llm"], remote["llm"]);
    assert_eq!(local["llm"]["config"]["reasoning_effort"], "low");
    assert_eq!(local["llm"]["config"]["is_reasoning_model"], true);
    assert_eq!(local["embedder"]["provider"], "huggingface");
}

#[test]
fn test_embedder_of_reads_the_switch() {
    assert_eq!(embedder_of(Some(&env(&[]))), "openrouter");
    assert_eq!(embedder_of(Some(&env(&[("EVE_MEM0_EMBEDDER", "\tGEMMA\n")]))), "gemma");
    assert_eq!(embedder_of(Some(&env(&[("EVE_MEM0_EMBEDDER", "ollama")]))), "openrouter");
}

#[test]
fn test_the_retired_ingest_is_gone() {
    let src = include_str!("mem0_memory.rs");
    assert!(src.contains("pub fn ingest_day")); // positive control
    for name in ["fn loop_", "fn ingest_pending", "fn pending_notes", "INGESTED_FILE", "NOTE_PREFIX", "fn note_body",
                 "INGEST_INTERVAL_SECS", "DEFAULT_HOME", "sessions_dir"] {
        assert!(!src.contains(name), "{name}");
    }
}

// ---- the helper process: its Python functions, then the protocol end to end ---------------------------------------

const HELPER_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/mem0_helper.py");

/// Runs `code` in python3 with the helper's functions loaded (its main does not run), returns stdout.
fn python_with_helper(code: &str, extra_env: &[(&str, &OsString)]) -> String {
    let script = format!(
        "import sys, types\nns = {{'__name__': 'mem0_helper'}}\nexec(open({:?}).read(), ns)\nns['_out'] = sys.stdout\n\
         globals().update(ns)\n{code}",
        HELPER_PATH
    );
    let mut cmd = Command::new("python3");
    cmd.arg("-c").arg(script).current_dir("/");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("python3 runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn test_the_helper_is_this_crates_source() {
    assert_eq!(HELPER, fs::read_to_string(HELPER_PATH).unwrap());
    assert!(HELPER.contains("if __name__ == \"__main__\":"));
}

#[test]
fn test_gemma_encodes_a_search_as_a_query_and_everything_stored_as_a_document() {
    let out = python_with_helper(
        r#"
import json
class Arr:
    def __init__(self, v): self.v = v
    def tolist(self): return self.v
class FakeST:
    def __init__(self): self.calls = []
    def encode(self, texts, prompt_name, normalize_embeddings, convert_to_numpy):
        self.calls.append((texts, prompt_name, normalize_embeddings))
        one = [1.0] * 3
        return Arr(one if isinstance(texts, str) else [one for _ in texts])
class FakeEmbedder:
    def __init__(self): self.model = FakeST()
emb = FakeEmbedder()
memory = type("M", (), {"embedding_model": emb})()
assert on_gemma(memory) is memory
assert emb.embed("where does my sister live", "search") == [1.0, 1.0, 1.0]
emb.embed("sister moved to the coast", "add")
emb.embed("sister moved inland", "update")
emb.embed("no action given")
assert emb.embed_batch(["a", "b"], "add") == [[1.0] * 3, [1.0] * 3]
emb.embed_batch(["c"], "search")
assert emb.embed_batch([], "add") == []
itself = FakeEmbedder()
on_gemma(itself).embed("x", "search")
print(json.dumps({"prompts": [c[1] for c in emb.model.calls], "normalised": all(c[2] for c in emb.model.calls),
                  "itself": itself.model.calls[0][1]}))
"#,
        &[],
    );
    let got: Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(got["prompts"], json!(["query", "document", "document", "document", "document", "query"]));
    assert_eq!(got["normalised"], true);
    assert_eq!(got["itself"], "query");
}

#[test]
fn test_meter_sends_each_reply_once_and_a_mem0_without_the_clients_is_left_alone() {
    let out = python_with_helper(
        r#"
M = 1_000_000
completions = types.SimpleNamespace(create=lambda **kw: {"usage": {"prompt_tokens": M, "completion_tokens": 0}})
embeddings = types.SimpleNamespace(create=lambda **kw: types.SimpleNamespace(
    usage=types.SimpleNamespace(model_dump=lambda exclude_none=False: {"prompt_tokens": M, "total_tokens": M}),
    data=[]))
memory = types.SimpleNamespace(
    llm=types.SimpleNamespace(client=types.SimpleNamespace(chat=types.SimpleNamespace(completions=completions))),
    embedding_model=types.SimpleNamespace(client=types.SimpleNamespace(embeddings=embeddings)))
init = {"embedder": "openrouter"}
assert meter(memory, init) is memory
meter(memory, init)
memory.llm.client.chat.completions.create(model="gpt-5.4-nano", messages=[])
memory.embedding_model.client.embeddings.create(model="text-embedding-3-small", input=["x"])
odd = types.SimpleNamespace()
assert meter(odd, init) is odd
"#,
        &[],
    );
    let lines: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let spends: Vec<&Value> = lines.iter().filter(|l| l.get("spend").is_some()).collect();
    assert_eq!(spends.len(), 2); // wrapped once, not twice
    let logs: Vec<&str> = lines.iter().filter_map(|l| l.get("message").and_then(Value::as_str)).collect();
    assert_eq!(
        logs,
        [
            "mem0: extraction calls are not metered (no create to wrap)",
            "mem0: embedding calls are not metered (no create to wrap)",
            "mem0: its model calls are not metered (this mem0 has no client where eve looks)",
        ]
    );
    let tmp = TempDir::new().unwrap();
    let ledger = eve_spend::Ledger::at(tmp.path());
    for s in spends {
        record_spend(&ledger, &Mem0Config::default(), s["spend"].as_str().unwrap(), s["model"].as_str().unwrap(), &s["reply"]);
    }
    let rows = ledger_lines(tmp.path());
    assert_eq!((rows[0]["f"].as_str(), rows[0]["m"].as_str()), (Some("memory"), Some("gpt-5.4-nano")));
    assert!((rows[0]["usd"].as_f64().unwrap() - 0.20).abs() < 1e-9);
    assert!((rows[1]["usd"].as_f64().unwrap() - 0.02).abs() < 1e-9);
}

/// Fake `mem0` and `openai` packages for driving the helper with the system python3: a Memory in RAM whose model
/// calls go through the openai client the helper installs, and an openai client that logs each request it builds.
struct FakePython {
    dir: TempDir,
}

const FAKE_TELEMETRY: &str = "import os\nMEM0_TELEMETRY = os.environ.get('MEM0_TELEMETRY', 'True').lower() in ('true', '1', 'yes')\n";

const FAKE_MEM0: &str = r#"
import os


class _LLM:
    def __init__(self, config):
        self.config = config
        self.client = None

    def generate_response(self, messages):
        return self.client.chat.completions.create(model=self.config["model"], messages=messages,
                                                   reasoning_effort=self.config["reasoning_effort"])


class _Embedder:
    def __init__(self, config):
        self.config = config
        self.client = None

    def embed(self, text, memory_action=None):
        self.client.embeddings.create(model=self.config["model"], input=[text])
        return [0.0]


class Memory:
    @classmethod
    def from_config(cls, config):
        m = cls()
        m.config = config
        m.llm = _LLM(config["llm"]["config"])
        m.embedding_model = _Embedder(config["embedder"]["config"])
        m.custom_instructions = config["custom_instructions"]
        m.rows = []
        os.makedirs(config["vector_store"]["config"]["path"], exist_ok=True)
        return m

    def add(self, messages, user_id, metadata):
        self.llm.generate_response(messages)
        self.embedding_model.embed(messages[-1]["content"], "add")
        self.rows.append({"id": "m%d" % (len(self.rows) + 1), "memory": messages[-1]["content"],
                          "metadata": metadata, "user_id": user_id})
        return {"results": []}

    def search(self, query, filters, top_k):
        self.embedding_model.embed(query, "search")
        return {"results": [dict(r, score=0.9) for r in self.rows][:top_k]}

    def get_all(self, filters, top_k):
        return {"results": list(self.rows)}

    def delete(self, memory_id):
        if memory_id not in [r["id"] for r in self.rows]:
            raise ValueError("Memory with id %s not found" % memory_id)
        self.rows = [r for r in self.rows if r["id"] != memory_id]
        return {"message": "Memory deleted successfully!"}
"#;

const FAKE_OPENAI: &str = r#"
import json
import os
import types


class _Options:
    def __init__(self, json_data):
        self.json_data = json_data


class _Usage:
    def __init__(self, d):
        self.d = d

    def model_dump(self, exclude_none=False):
        return dict(self.d)


class _Completions:
    def __init__(self, client):
        self._client = client

    def create(self, **kw):
        self._client._build_request(_Options(kw))
        return {"usage": {"prompt_tokens": 1000000, "completion_tokens": 0}}


class _Embeddings:
    def __init__(self, client):
        self._client = client

    def create(self, **kw):
        self._client._build_request(_Options(kw))
        return types.SimpleNamespace(usage=_Usage({"prompt_tokens": 1000000, "total_tokens": 1000000}), data=[])


class OpenAI:
    def __init__(self, api_key, base_url, default_headers=None):
        self.api_key = api_key
        self.base_url = base_url
        self.default_headers = default_headers or {}
        self.chat = types.SimpleNamespace(completions=_Completions(self))
        self.embeddings = _Embeddings(self)

    def _build_request(self, options, *, retries_taken=0):
        with open(os.environ["FAKE_OPENAI_LOG"], "a") as f:
            f.write(json.dumps({"base_url": self.base_url, "headers": self.default_headers,
                                "key": self.api_key, "body": options.json_data}) + "\n")
        return options
"#;

impl FakePython {
    fn new(telemetry_stuck_on: bool) -> Self {
        let dir = TempDir::new().unwrap();
        let lib = dir.path().join("lib");
        fs::create_dir_all(lib.join("mem0").join("memory")).unwrap();
        fs::create_dir_all(lib.join("openai")).unwrap();
        fs::write(lib.join("mem0").join("__init__.py"), FAKE_MEM0).unwrap();
        fs::write(lib.join("mem0").join("memory").join("__init__.py"), "").unwrap();
        let telemetry = if telemetry_stuck_on { "MEM0_TELEMETRY = True\n" } else { FAKE_TELEMETRY };
        fs::write(lib.join("mem0").join("memory").join("telemetry.py"), telemetry).unwrap();
        fs::write(lib.join("openai").join("__init__.py"), FAKE_OPENAI).unwrap();
        FakePython { dir }
    }

    fn log(&self) -> PathBuf {
        self.dir.path().join("requests.jsonl")
    }

    fn env(&self, key: &str) -> Vec<(OsString, OsString)> {
        vec![
            ("PYTHONPATH".into(), self.dir.path().join("lib").into_os_string()),
            ("PYTHONNOUSERSITE".into(), "1".into()),
            ("FAKE_OPENAI_LOG".into(), self.log().into_os_string()),
            ("OPENROUTER_API_KEY".into(), key.into()),
            ("MEM0_TELEMETRY".into(), "True".into()), // what eve forces off before the import
        ]
    }

    fn requests(&self) -> Vec<Value> {
        fs::read_to_string(self.log())
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn open(&self, cfg: &Mem0Config, ledger: &eve_spend::Ledger) -> anyhow::Result<PythonMem0> {
        PythonMem0::open(cfg.clone(), "python3", self.env("or-test-not-real"), ledger.clone())
    }
}

#[test]
fn test_telemetry_is_off_and_mem0_home_is_ours_before_the_import() {
    let fake = FakePython::new(false);
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("mem0");
    let cfg = Mem0Config::new(true, &home);
    let mut real = fake.open(&cfg, &eve_spend::Ledger::at(tmp.path().join("spend"))).unwrap();
    let probe = real.call(json!({"op": "probe"})).unwrap();
    assert_eq!(probe["telemetry"], false);
    assert_eq!(probe["mem0_dir"], home.join("home").to_str().unwrap());
    assert!(home.join("qdrant").is_dir());
    assert_eq!(probe["custom_instructions"], CUSTOM_INSTRUCTIONS); // the key mem0 actually reads
    assert_eq!(probe["llm_base_url"], eve_openrouter::BASE_URL);
    assert_eq!(probe["embedder_base_url"], eve_openrouter::BASE_URL);
}

#[test]
fn test_a_mem0_whose_telemetry_stays_on_is_refused() {
    let fake = FakePython::new(true);
    let tmp = TempDir::new().unwrap();
    let err = fake.open(&Mem0Config::new(true, tmp.path().join("m")), &eve_spend::Ledger::at(tmp.path())).unwrap_err();
    let e = err.downcast_ref::<Mem0Error>().unwrap();
    assert_eq!(e.kind, "RuntimeError");
    assert!(e.message.contains("telemetry is on"));
}

#[test]
fn test_no_openrouter_key_is_no_mem0() {
    let fake = FakePython::new(false);
    let tmp = TempDir::new().unwrap();
    let err = PythonMem0::open(Mem0Config::new(true, tmp.path().join("m")), "python3", fake.env("  "), eve_spend::Ledger::at(tmp.path()))
        .unwrap_err();
    assert_eq!(error_name(&err), "NoKey");
    let missing = PythonMem0::open(Mem0Config::new(true, tmp.path().join("m")), "/nonexistent/python3", Vec::new(),
        eve_spend::Ledger::at(tmp.path())).unwrap_err();
    assert_eq!(error_name(&missing), "FileNotFoundError");
}

#[test]
fn test_extraction_and_embeddings_go_to_openrouter_metered_with_the_extraction_budget() {
    let fake = FakePython::new(false);
    let tmp = TempDir::new().unwrap();
    let spend = tmp.path().join("spend");
    let cfg = Mem0Config { model: "gpt-5.4-nano".into(), ..Mem0Config::new(true, tmp.path().join("m")) };
    let mut real = fake.open(&cfg, &eve_spend::Ledger::at(&spend)).unwrap();
    real.call(json!({"op": "generate", "messages": [{"role": "user", "content": "hi"}]})).unwrap();
    assert!(real.embed("hello", "search").unwrap());
    let sent = fake.requests();
    assert_eq!(sent.len(), 2);
    let (chat, emb) = (&sent[0], &sent[1]);
    assert_eq!(chat["base_url"], eve_openrouter::BASE_URL);
    assert_eq!(chat["key"], "or-test-not-real");
    assert_eq!(chat["headers"]["X-Title"], "eve");
    assert_eq!(chat["body"]["model"], "openai/gpt-5.4-nano"); // the builder adds "openai/"
    assert_eq!(chat["body"]["reasoning_effort"], "low");
    assert_eq!(chat["body"]["max_completion_tokens"], EXTRACT_MAX_OUTPUT_TOKENS);
    assert_eq!(emb["body"]["model"], "openai/text-embedding-3-small");
    let rows = ledger_lines(&spend);
    assert_eq!(rows.len(), 2);
    assert_eq!((rows[0]["f"].as_str(), rows[0]["p"].as_str(), rows[0]["m"].as_str()),
               (Some("memory"), Some("openrouter"), Some("gpt-5.4-nano")));
    assert!((rows[0]["usd"].as_f64().unwrap() - 0.20).abs() < 1e-9);
    assert_eq!(rows[1]["m"], "text-embedding-3-small");
    assert!((rows[1]["usd"].as_f64().unwrap() - 0.02).abs() < 1e-9);
    // never a real key in mem0's config
    assert_eq!(mem0_config(&cfg)["llm"]["config"]["api_key"], PLACEHOLDER_KEY);
}

#[test]
fn test_gemma_keeps_extraction_on_openrouter_and_metered_and_meters_no_embeddings() {
    let fake = FakePython::new(false);
    let out = python_with_helper(
        r#"
import json
llm = type("L", (), {"client": None})()
emb = type("E", (), {})()
memory = type("M", (), {"llm": llm, "embedding_model": emb})()
init = {"key_env": "OPENROUTER_API_KEY", "base_url": "https://openrouter.ai/api/v1", "headers": {}, "models": {},
        "extract_max_output_tokens": 2000, "embedder": "gemma"}
on_openrouter(memory, init, embeddings=False)
meter(memory, init)
print(json.dumps({"base_url": str(llm.client.base_url), "emb_client": hasattr(emb, "client"),
                  "metered": getattr(llm.client.chat.completions.create, "_eve_metered", False)}))
"#,
        &[
            ("PYTHONPATH", &fake.dir.path().join("lib").into_os_string()),
            ("OPENROUTER_API_KEY", &OsString::from("or-test-not-real")),
        ],
    );
    let got: Value = serde_json::from_str(out.lines().last().unwrap()).unwrap();
    assert!(got["base_url"].as_str().unwrap().starts_with("https://openrouter.ai/api/v1"));
    assert_eq!(got["emb_client"], false); // embeddings never leave the Mac
    assert_eq!(got["metered"], true);
    assert!(!out.contains("not metered"));
}

#[test]
fn test_the_index_over_the_helper_ingests_searches_and_forgets() {
    let fake = FakePython::new(false);
    let tmp = TempDir::new().unwrap();
    let spend = tmp.path().join("spend");
    let factory: Factory = {
        let (env, spend) = (fake.env("or-test-not-real"), spend.clone());
        Arc::new(move |cfg: &Mem0Config| {
            Ok(Box::new(PythonMem0::open(cfg.clone(), "python3", env.clone(), eve_spend::Ledger::at(&spend))?)
                as Box<dyn Mem0Backend>)
        })
    };
    let mem = OwnerMemory::new(Mem0Config::new(true, tmp.path().join("m")), Some(factory));
    mem.set_search_timeout(10.0);
    let (t, _) = transcripts_with_a_day(tmp.path());
    assert_eq!(mem.ingest_day(&t, DAY), 2);
    let hits = mem.search("sister", MAX_HITS);
    assert_eq!(hits[0]["text"], "Lovely, which town?");
    assert_eq!(hits[0]["channel"], "voice");
    assert_eq!(hits[0]["day"], DAY);
    let found = mem.find(&|t| t.contains("noted"));
    assert_eq!(found.len(), 1);
    assert_eq!(mem.forget(&[found[0].id.as_str(), "m99"]), Ok(1)); // the missing one is logged, not counted
    assert_eq!(mem.find(&|_| true).len(), 1);
    mem.warm();
    // two adds and one search and the warm-up: each model call metered
    assert_eq!(ledger_lines(&spend).len(), 2 * 2 + 1 + 1);
    assert_eq!(mode(&tmp.path().join("m")), 0o700);
}

/// The real mem0ai, opened offline with a fake key: telemetry off, its home ours, its clients OpenRouter's, and a
/// forget that scrubs mem0's own history database. Needs an interpreter with mem0ai (EVE_MEM0_PYTHON, or the
/// bridge venv when it has it).
#[test]
#[ignore = "live: needs mem0ai (EVE_MEM0_PYTHON or the bridge venv)"]
fn test_real_mem0_opens_with_telemetry_off_and_its_home_ours() {
    let python = crate::python::default_python();
    assert!(crate::python::available_with(&python), "mem0ai is not installed for {}", python.display());
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("mem0");
    let cfg = Mem0Config::new(true, &home);
    let env: Vec<(OsString, OsString)> = vec![
        ("OPENROUTER_API_KEY".into(), "or-test-not-real".into()),
        ("OPENAI_API_KEY".into(), "".into()),
        ("HOME".into(), tmp.path().as_os_str().to_owned()),
    ];
    let mut real = PythonMem0::open(cfg.clone(), &python, env.clone(), eve_spend::Ledger::at(tmp.path().join("spend")))
        .unwrap();
    let probe = real.call(json!({"op": "probe"})).unwrap();
    assert_eq!(probe["telemetry"], false);
    assert_eq!(probe["mem0_dir"], home.join("home").to_str().unwrap());
    assert!(home.join("qdrant").is_dir());
    assert!(!tmp.path().join(".mem0").exists());
    assert_eq!(probe["custom_instructions"], CUSTOM_INSTRUCTIONS);
    assert!(probe["llm_base_url"].as_str().unwrap().starts_with(eve_openrouter::BASE_URL));
    assert!(probe["embedder_base_url"].as_str().unwrap().starts_with(eve_openrouter::BASE_URL));
    drop(real);
    let factory: Factory = Arc::new(move |cfg: &Mem0Config| {
        Ok(Box::new(PythonMem0::open(cfg.clone(), &python, env.clone(), eve_spend::Ledger::default())?) as Box<dyn Mem0Backend>)
    });
    let mem = OwnerMemory::new(cfg, Some(factory));
    assert!(mem.find(&|_| true).is_empty()); // an empty store lists nothing, offline
    assert_eq!(mem.tidy(), 0);
    assert_eq!(mem.forget_matching(&|_| true), Ok(0));
}
