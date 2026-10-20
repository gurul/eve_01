//! test_memory_search.py, the Memory side: the search tool's shape per switch, forget reaching the ranked index
//! (switch on, and a file left with it off), a forget the index cannot finish, the worker only in the daemon, a
//! time range that narrows and widens, and the code-side retry. The index itself is eve-transcripts'.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Value};

use eve_records::RecallConfig;
use eve_transcripts::memory_index::{self as mi, Embedder};
use eve_transcripts::transcripts::{Dt, Transcripts};

use crate::memory::{memory_search_tool, Memory, MemoryOptions, MEMORY_SEARCH};
use crate::testutil::{at_tz, say, tmp, transcripts, tz_on};

const SENTINEL: &str = "quokka-sentinel-9Z";

fn at(month: u32, day: u32, hh: u32, mm: u32) -> Dt {
    at_tz(tz_on(2026, 9, 21), 2026, month, day, hh, mm)
}

/// Bag of words (one dimension per word, no collisions), with a few synonyms folded together: meaning without a
/// network.
#[derive(Clone, Default)]
struct FakeEmbedder {
    calls: Arc<AtomicUsize>,
    vocab: Arc<Mutex<HashMap<String, usize>>>,
}

impl FakeEmbedder {
    fn embedder(&self) -> Embedder {
        let me = self.clone();
        let word = Regex::new(r"\w+").unwrap();
        let synonyms: HashMap<&str, &str> =
            [("bicycle", "bike"), ("cycle", "bike"), ("stole", "theft"), ("stolen", "theft")].into_iter().collect();
        Arc::new(move |texts: &[String], _timeout: f64| {
            me.calls.fetch_add(1, Ordering::SeqCst);
            let mut out = Vec::new();
            for t in texts {
                let mut v = vec![0.0f64; 8192];
                let lower = t.to_lowercase();
                for w in word.find_iter(&lower).map(|m| m.as_str()) {
                    if mi::STOPWORDS.contains(w) || w == "owner" || w == "eve" {
                        continue;
                    }
                    let stem = w.trim_end_matches('s');
                    let key = synonyms.get(stem).copied().unwrap_or(stem).to_string();
                    let mut vocab = me.vocab.lock();
                    let n = vocab.len();
                    let i = *vocab.entry(key).or_insert(n % 8192);
                    v[i] += 1.0;
                }
                out.push(v);
            }
            Ok(out)
        })
    }
}

fn seed_lines(tr: &Transcripts) {
    say(tr, "telegram", "t-1", "owner", "Carla, my landlord, says the rent includes water.", at(9, 15, 10, 0));
    say(tr, "telegram", "t-1", "eve", "Good to know, so only internet is extra.", at(9, 15, 10, 1));
    say(tr, "telegram", "t-2", "owner", "Someone stole my bicycle from outside Kellner Hall.", at(9, 19, 9, 0));
    say(tr, "telegram", "t-2", "eve", "I'm sorry. Look for a used one under $150.", at(9, 19, 9, 1));
    say(tr, "voice", "v-3", "owner", "Squatted 245 today.", at(9, 20, 18, 0));
    say(tr, "telegram", "t-4", "owner", "Ordered pad thai again.", at(9, 24, 12, 0));
}

fn switches(hybrid: bool, timeargs: bool, retry: bool) -> HashMap<String, String> {
    [(mi::HYBRID_ENV, hybrid), (mi::TIMEARGS_ENV, timeargs), (mi::RETRY_ENV, retry)]
        .into_iter()
        .map(|(k, on)| (k.to_string(), if on { "1" } else { "0" }.to_string()))
        .collect()
}

/// test_memory_search.py's `memory`: transcripts under `<tmp>/mem/transcripts`, the real records, no mem0.
fn memory(tmp: &Path, now: Dt, hybrid: bool, timeargs: bool, retry: bool, background: bool) -> Memory {
    let store = tmp.join("mem");
    let cfg = RecallConfig::new(store.clone(), store.join("notes"));
    let tr = transcripts(&cfg, now);
    seed_lines(&tr);
    let opts = MemoryOptions {
        environ: Some(switches(hybrid, timeargs, retry)),
        embedder: Some(FakeEmbedder::default().embedder()),
        background,
        ..MemoryOptions::default()
    };
    let m = Memory::with_options(cfg, tr, None, opts);
    if let Some(r) = &m.ranked {
        r.ensure_built();
    }
    m
}

fn file_holds(path: &Path, word: &str) -> bool {
    let raw = std::fs::read(path).unwrap().to_ascii_lowercase();
    let w = word.to_ascii_lowercase().into_bytes();
    raw.windows(w.len()).any(|win| win == w.as_slice())
}

fn texts(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|h| h["text"].as_str().unwrap_or("").to_string()).collect()
}

fn days(v: &Value) -> Vec<String> {
    v.as_array().unwrap().iter().map(|h| h["day"].as_str().unwrap_or("").to_string()).collect()
}

// ---- the switches ----------------------------------------------------------------------------------------------

#[test]
fn test_tool_schema_all_off_is_the_old_one_and_each_switch_shapes_it() {
    assert_eq!(memory_search_tool(false, false, false, true), *MEMORY_SEARCH);
    let ranked = memory_search_tool(true, false, false, true);
    let desc = ranked["description"].as_str().unwrap();
    assert!(!desc.contains("Every word of the query") && desc.contains("best match first"));
    let timed = memory_search_tool(false, true, false, true);
    let params = &timed["parameters"];
    assert_eq!(params["required"], json!(["query", "days_back", "from", "to"]));
    assert_eq!(params["properties"]["from"]["type"], json!(["string", "null"]));
    let desc = timed["description"].as_str().unwrap();
    assert!(desc.contains("Every word of the query") && desc.contains("pass it as from and to"));
    let retry = memory_search_tool(false, false, true, true);
    assert!(retry["description"].as_str().unwrap().contains("search once more with other words"));
    // the cap is stated only where code enforces it: the Telegram brain, not voice
    assert!(retry["description"].as_str().unwrap().contains("at most three memory calls"));
    let uncapped = memory_search_tool(false, false, true, false);
    assert!(!uncapped["description"].as_str().unwrap().contains("at most three memory calls"));
    // fixed per switch set: the cached prefix does not move between calls
    assert_eq!(
        eve_transcripts::py::dumps(&memory_search_tool(true, true, true, true)),
        eve_transcripts::py::dumps(&memory_search_tool(true, true, true, true))
    );
}

#[test]
fn test_voice_is_not_told_of_a_cap_it_does_not_have() {
    let d = tmp();
    let m = memory(d.path(), at(9, 24, 20, 0), false, false, true, false);
    assert!(m.tools(false)[0]["description"].as_str().unwrap().contains("at most three memory calls"));
    assert!(!m.tools(true)[0]["description"].as_str().unwrap().contains("at most three memory calls"));
}

// ---- the Memory: forget, ranges, retry ---------------------------------------------------------------------------

#[test]
fn test_forget_reaches_the_index_rows_and_vectors() {
    let d = tmp();
    let m = memory(d.path(), at(9, 24, 20, 0), true, false, false, false);
    let ranked = m.ranked.clone().expect("the hybrid switch opens the index");
    say(&m.transcripts, "telegram", "t-9", "owner", &format!("My locker code is {SENTINEL}."), at(9, 24, 18, 0));
    say(&m.transcripts, "telegram", "t-9", "eve", "Got it.", at(9, 24, 18, 1));
    ranked.ensure_built();
    let path = mi::index_path(&m.cfg.store);
    assert!(file_holds(&path, SENTINEL)); // the positive control
    assert!(!m.search(SENTINEL, None)["said"].as_array().unwrap().is_empty());
    let prev = m.forget_preview(SENTINEL);
    assert_eq!(prev["layers"]["index"], json!(1));
    let done = m.forget_apply(prev["token"].as_str().unwrap());
    assert!(done["layers"]["index"] == json!(1) && done["layers"]["transcripts"] == json!(2));
    assert!(!file_holds(&path, SENTINEL));
    let orphans = ranked
        .with_db(|db| {
            db.query_row("SELECT COUNT(*) FROM vectors WHERE hash NOT IN (SELECT hash FROM units)", [], |r| {
                r.get::<_, i64>(0)
            })
        })
        .unwrap()
        .unwrap();
    assert_eq!(orphans, 0); // no vector outlives its round
    assert!(m.search(SENTINEL, None)["said"].as_array().unwrap().is_empty());
    assert!(m.search("Got it", None)["said"].as_array().unwrap().is_empty()); // eve's answer to it went too
    ranked.close();
}

#[test]
fn test_forget_reaches_words_past_the_first_2000_chars_of_a_round() {
    let d = tmp();
    let m = memory(d.path(), at(9, 24, 20, 0), true, false, false, false);
    let ranked = m.ranked.clone().unwrap();
    say(&m.transcripts, "telegram", "t-9", "owner", "Tell me a long story please.", at(9, 24, 18, 0));
    let long = format!("{} the secret is {SENTINEL}", "filler words here ".repeat(130));
    say(&m.transcripts, "telegram", "t-9", "eve", &long, at(9, 24, 18, 1));
    ranked.ensure_built();
    let path = mi::index_path(&m.cfg.store);
    assert!(file_holds(&path, SENTINEL)); // the positive control: the reply is stored
    let prev = m.forget_preview(SENTINEL);
    assert_eq!(prev["layers"]["index"], json!(1)); // counted past the FTS body's clip
    let done = m.forget_apply(prev["token"].as_str().unwrap());
    assert!(done["ok"] == json!(true) && done["layers"]["index"] == json!(1));
    assert!(!file_holds(&path, SENTINEL));
    ranked.close();
}

#[test]
fn test_forget_cleans_an_index_left_on_disk_with_hybrid_off() {
    let d = tmp();
    let on = memory(d.path(), at(9, 24, 20, 0), true, false, false, false);
    let ranked = on.ranked.clone().unwrap();
    say(&on.transcripts, "telegram", "t-9", "owner", &format!("My locker code is {SENTINEL}."), at(9, 24, 18, 0));
    ranked.ensure_built();
    ranked.close();
    let path = mi::index_path(&on.cfg.store);
    assert!(file_holds(&path, SENTINEL)); // the positive control: the file holds it
    let off = Memory::with_options(
        on.cfg.clone(),
        on.transcripts.clone(),
        None,
        MemoryOptions { environ: Some(switches(false, false, false)), ..MemoryOptions::default() },
    );
    assert!(off.ranked.is_none());
    let prev = off.forget_preview(SENTINEL);
    assert_eq!(prev["layers"]["index"], json!(1)); // the switch is off, the file is counted
    let done = off.forget_apply(prev["token"].as_str().unwrap());
    assert!(done["ok"] == json!(true) && done["layers"]["index"] == json!(1));
    assert!(!file_holds(&path, SENTINEL));
}

#[test]
fn test_a_forget_that_cannot_clean_the_index_says_so() {
    let d = tmp();
    let m = memory(d.path(), at(9, 24, 20, 0), true, false, false, false);
    let ranked = m.ranked.clone().unwrap();
    ranked.tuning.write().compact_waits = vec![0.0, 0.0];
    say(&m.transcripts, "telegram", "t-9", "owner", &format!("My locker code is {SENTINEL}."), at(9, 24, 18, 0));
    ranked.ensure_built();
    // the index refuses the delete, as a file another process holds would
    let refused = ranked
        .with_db(|db| {
            db.execute_batch(
                "CREATE TEMP TRIGGER refuse BEFORE DELETE ON units BEGIN SELECT RAISE(ABORT, 'database is locked'); END;",
            )
        })
        .unwrap();
    assert!(refused.is_ok());
    let prev = m.forget_preview(SENTINEL);
    assert_eq!(prev["layers"]["index"], json!(1));
    let done = m.forget_apply(prev["token"].as_str().unwrap());
    assert!(done["ok"] == json!(false) && done["failed"] == json!(["index"]));
    assert!(done["reason"].as_str().unwrap().contains("index"));
    ranked.close();
}

#[test]
fn test_only_the_daemon_starts_the_index_worker() {
    let d = tmp();
    let m = memory(d.path(), at(9, 24, 20, 0), true, false, false, false);
    let ranked = m.ranked.clone().unwrap();
    assert!(!ranked.has_worker()); // the CLI and the eval: no worker
    // Review D10: a search before the index is built must not start the build-and-embed worker either.
    ranked.set_built(false); // as for a missing index file
    assert!(ranked.search("rent", "2026-09-01", "2026-09-24", mi::TOP_K, None).is_none());
    assert!(!ranked.has_worker());
    ranked.close();
    let daemon = Memory::with_options(
        m.cfg.clone(),
        m.transcripts.clone(),
        None,
        MemoryOptions {
            environ: Some(switches(true, false, false)),
            embedder: Some(FakeEmbedder::default().embedder()),
            background: true,
            ..MemoryOptions::default()
        },
    );
    let worker = daemon.ranked.clone().unwrap();
    assert!(worker.has_worker());
    worker.close();
}

#[test]
fn test_with_hybrid_on_the_transcripts_search_is_ranked_and_new_lines_reach_the_index() {
    let d = tmp();
    let m = memory(d.path(), at(9, 24, 20, 0), true, false, false, false);
    // the ranker is attached: a query no line holds word for word is found by meaning
    let got = m.search("bike theft", None);
    assert!(texts(&got["said"])[0].starts_with("Someone stole my bicycle"));
    // the listener is attached: the index hears the day a new line is written on (no worker: it just marks it)
    say(&m.transcripts, "telegram", "t-5", "owner", "The new apartment wifi password is on the fridge.", at(9, 24, 19, 0));
    let got = m.search("wifi password", None);
    assert!(texts(&got["said"])[0].starts_with("The new apartment wifi"));
    m.ranked.clone().unwrap().close();
}

#[test]
fn test_a_range_limits_the_words_and_widens_once() {
    let d = tmp();
    let m = memory(d.path(), at(9, 24, 20, 0), false, true, false, false);
    let ask = |args: Value| m.handle_tool("memory_search", &args, None, "");
    let got = ask(json!({"query": "squatted", "days_back": null, "from": "4 days ago", "to": null}));
    assert_eq!(got["range"], json!({"from": "2026-09-20", "to": "2026-09-20"}));
    assert!(got.get("widened").is_none());
    assert_eq!(days(&got["said"]), ["2026-09-20"]);
    let got = ask(json!({"query": "squatted", "days_back": null, "from": "yesterday", "to": null}));
    assert_eq!(got["widened"], json!({"from": "2026-09-16", "to": "2026-09-24"}));
    assert!(!got["said"].as_array().unwrap().is_empty());
    let got = ask(json!({"query": "squat weight", "days_back": null, "from": "4 days ago", "to": null}));
    assert!(got["fewer_words"] == json!(true) && got.get("widened").is_none()); // AND misses "weight"; the range
    assert_eq!(texts(&got["said"]), ["Squatted 245 today."]); // keeps half the words narrow
    let got = ask(json!({"query": "squatted", "days_back": null, "from": "a while back", "to": null}));
    assert_eq!(got["time_note"], json!("a time could not be read as a date; searched without it"));
    assert!(!got["said"].as_array().unwrap().is_empty());
    // a falsy from is no time at all, as Python reads it
    let got = ask(json!({"query": "squatted", "days_back": null, "from": "", "to": null}));
    assert!(got.get("range").is_none() && got.get("time_note").is_none());
    let off_dir = d.path().join("off");
    let off = memory(&off_dir, at(9, 24, 20, 0), false, false, false, false);
    let got = off.handle_tool("memory_search", &json!({"query": "squatted", "days_back": null, "from": "yesterday"}), None, "");
    assert!(got.get("range").is_none()); // switch off: from/to are not read
    assert!(!got["said"].as_array().unwrap().is_empty());
}

#[test]
fn test_retry_searches_again_wider_and_looser() {
    let d = tmp();
    let m = memory(d.path(), at(9, 24, 20, 0), false, false, true, false);
    say(&m.transcripts, "telegram", "t-old", "owner", "The garage door code changed to 4471.", at(3, 2, 9, 0));
    let got = m.search("garage door code", None); // 206 days back: past the 30
    assert!(!got["said"].as_array().unwrap().is_empty());
    assert!(got["searched_again"].as_str().unwrap().starts_with("the last 365 days"));
    let got = m.search("landlord name", None); // AND misses; half the words find it
    assert!(texts(&got["said"])[0].starts_with("Carla"));
    assert!(got["searched_again"].as_str().unwrap().contains("fewer words"));
    let got = m.search("zeppelin hangar", None);
    assert!(got["said"].as_array().unwrap().is_empty());
    assert!(got["note"].as_str().unwrap().contains("search once more with other words"));
    let off_dir = d.path().join("off");
    let off = memory(&off_dir, at(9, 24, 20, 0), false, false, false, false);
    assert!(off.search("landlord name", None)["said"].as_array().unwrap().is_empty());
    assert!(off.search("landlord name", None).get("searched_again").is_none());
    assert_eq!(off.search("landlord name", None)["note"], json!("nothing matched"));
}
