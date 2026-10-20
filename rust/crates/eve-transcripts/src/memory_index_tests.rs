//! memory_index.py's tests: the index-level part of bridge/tests/test_memory_search.py (the switches, the time
//! resolver across the 04:00 line, units, the ranked index and its fallback, forget reaching it) and
//! bridge/tests/test_memory_verdict.py's memory_index rows. The Memory-level and Telegram tests of that file belong to
//! the memory and telegram crates.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration as StdDuration;

use chrono::{Datelike, FixedOffset, Local, NaiveDate, Offset, TimeZone};
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};

use crate::memory_index::{self as mi, Embedder, MemoryIndex};
use crate::py;
use crate::transcripts::{match_query, speaker, Append, Dt, Search, TranscriptConfig, Transcripts};

const SENTINEL: &str = "quokka-sentinel-9Z";

fn tz() -> FixedOffset {
    Local
        .with_ymd_and_hms(2026, 9, 21, 12, 0, 0)
        .unwrap()
        .offset()
        .fix()
}

fn at(month: u32, day: u32, hh: u32, mm: u32) -> Dt {
    tz().with_ymd_and_hms(2026, month, day, hh, mm, 0).unwrap()
}

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// Bag of words (one dimension per word, no collisions), with a few synonyms folded together: meaning without a
/// network.
#[derive(Default)]
struct FakeState {
    calls: usize,
    texts: usize,
    vocab: HashMap<String, usize>,
    fail: bool,
}

#[derive(Clone, Default)]
struct FakeEmbedder(Arc<Mutex<FakeState>>);

impl FakeEmbedder {
    fn embedder(&self) -> Embedder {
        let state = self.0.clone();
        let word = Regex::new(r"\w+").unwrap();
        Arc::new(move |texts: &[String], _timeout: f64| {
            let mut s = state.lock();
            if s.fail {
                anyhow::bail!("embeddings down");
            }
            s.calls += 1;
            s.texts += texts.len();
            let mut out = Vec::new();
            for t in texts {
                let mut v = vec![0.0f64; 8192];
                for w in word.find_iter(&t.to_lowercase()).map(|m| m.as_str()) {
                    if mi::STOPWORDS.contains(w) || w == "owner" || w == "eve" {
                        continue;
                    }
                    let base = w.trim_end_matches('s');
                    let w = match base {
                        "bicycle" | "cycle" => "bike",
                        "stole" | "stolen" => "theft",
                        other => other,
                    }
                    .to_string();
                    let next = s.vocab.len() % 8192;
                    let i = *s.vocab.entry(w).or_insert(next);
                    v[i] += 1.0;
                }
                out.push(v);
            }
            Ok(out)
        })
    }

    fn texts(&self) -> usize {
        self.0.lock().texts
    }

    fn set_fail(&self, fail: bool) {
        self.0.lock().fail = fail;
    }
}

fn make_store(tmp: &Path, now: Dt) -> Arc<Transcripts> {
    let tr = Transcripts::new(
        TranscriptConfig::new(true, tmp.join("mem").join("transcripts")),
        None,
        Some(Arc::new(move || now)),
    );
    assert!(tr.enabled);
    Arc::new(tr)
}

fn say_on(tr: &Transcripts, conv: &str, who: &str, text: &str, when: Dt, ch: &str) {
    assert!(tr.append_with(
        ch,
        conv,
        who,
        "say",
        text,
        Append {
            now: Some(when),
            ..Append::default()
        }
    ));
}

fn say(tr: &Transcripts, conv: &str, who: &str, text: &str, when: Dt) {
    say_on(tr, conv, who, text, when, "telegram");
}

fn seed_lines(tr: &Transcripts) {
    say(
        tr,
        "t-1",
        "owner",
        "Carla, my landlord, says the rent includes water.",
        at(9, 15, 10, 0),
    );
    say(
        tr,
        "t-1",
        "eve",
        "Good to know, so only internet is extra.",
        at(9, 15, 10, 1),
    );
    say(
        tr,
        "t-2",
        "owner",
        "Someone stole my bicycle from outside Kellner Hall.",
        at(9, 19, 9, 0),
    );
    say(
        tr,
        "t-2",
        "eve",
        "I'm sorry. Look for a used one under $150.",
        at(9, 19, 9, 1),
    );
    say_on(
        tr,
        "v-3",
        "owner",
        "Squatted 245 today.",
        at(9, 20, 18, 0),
        "voice",
    );
    say(
        tr,
        "t-4",
        "owner",
        "Ordered pad thai again.",
        at(9, 24, 12, 0),
    );
}

fn index_for(tr: &Arc<Transcripts>, emb: &FakeEmbedder) -> Arc<MemoryIndex> {
    let source: Arc<dyn mi::DaySource> = tr.clone();
    let idx = Arc::new(MemoryIndex::new(
        mi::index_path(tr.root.parent().unwrap()),
        source,
        Some(emb.embedder()),
    ));
    assert!(idx.ok());
    tr.set_ranker(Some(idx.clone()));
    idx
}

fn hits(res: &Value) -> Vec<Value> {
    res["hits"].as_array().cloned().unwrap_or_default()
}

fn text_of(h: &Value) -> &str {
    h["text"].as_str().unwrap_or("")
}

fn wait_until(mut done: impl FnMut() -> bool, tries: usize, step_ms: u64) -> bool {
    for _ in 0..tries {
        if done() {
            return true;
        }
        std::thread::sleep(StdDuration::from_millis(step_ms));
    }
    done()
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

// ---- the switches -----------------------------------------------------------------------------------------------------

#[test]
fn test_switches_are_off_unless_set() {
    let env = |k: &str, v: &str| HashMap::from([(k.to_string(), v.to_string())]);
    assert!(!mi::switch("hybrid", Some(&HashMap::new())));
    assert!(mi::switch("hybrid", Some(&env(mi::HYBRID_ENV, "1"))));
    assert!(mi::switch(mi::RETRY_ENV, Some(&env(mi::RETRY_ENV, "on"))));
    assert!(!mi::switch("timeargs", Some(&env(mi::TIMEARGS_ENV, "0"))));
}

// ---- the time resolver --------------------------------------------------------------------------------------------------

#[test]
fn test_resolver_uses_the_owners_day_across_the_0400_line() {
    let before = tz().with_ymd_and_hms(2026, 9, 10, 2, 30, 0).unwrap(); // 02:30 on the 10th: still the owner's 9th
    let after = tz().with_ymd_and_hms(2026, 9, 10, 4, 30, 0).unwrap();
    assert_eq!(mi::owner_day(before), d("2026-09-09"));
    assert_eq!(mi::owner_day(after), d("2026-09-10"));
    assert_eq!(
        mi::resolve_when("yesterday", before),
        Some((d("2026-09-08"), d("2026-09-08")))
    );
    assert_eq!(
        mi::resolve_when("yesterday", after),
        Some((d("2026-09-09"), d("2026-09-09")))
    );
    assert_eq!(
        mi::resolve_when("today", before),
        Some((d("2026-09-09"), d("2026-09-09")))
    );
    assert_eq!(
        mi::resolve_when("last night", after),
        Some((d("2026-09-09"), d("2026-09-09")))
    );
    assert_eq!(
        mi::resolve_when("3 days ago", before),
        Some((d("2026-09-06"), d("2026-09-06")))
    );
}

#[test]
fn test_resolver_weeks_months_and_dates() {
    let now = at(9, 23, 20, 0); // Wednesday 2026-09-23
    assert_eq!(d("2026-09-23").weekday().num_days_from_monday(), 2);
    let r = |s: &str| mi::resolve_when(s, now);
    let span = |a: &str, b: &str| Some((d(a), d(b)));
    assert_eq!(r("last week"), span("2026-09-14", "2026-09-20"));
    assert_eq!(r("this week"), span("2026-09-21", "2026-09-23"));
    assert_eq!(r("two weeks ago"), span("2026-09-07", "2026-09-13"));
    assert_eq!(r("in May"), span("2026-05-01", "2026-05-31"));
    assert_eq!(r("in December"), span("2025-12-01", "2025-12-31")); // never in the future
    assert_eq!(r("this month"), span("2026-09-01", "2026-09-23"));
    assert_eq!(r("last month"), span("2026-08-01", "2026-08-31"));
    assert_eq!(r("early August"), span("2026-08-01", "2026-08-10"));
    assert_eq!(r("August 6"), span("2026-08-06", "2026-08-06"));
    assert_eq!(r("6th of August"), span("2026-08-06", "2026-08-06"));
    assert_eq!(r("since August 6"), span("2026-08-06", "2026-09-23"));
    assert_eq!(r("2026-09-03"), span("2026-09-03", "2026-09-03"));
    assert_eq!(r("last Tuesday"), span("2026-09-22", "2026-09-22"));
    assert_eq!(r("on Wednesday"), span("2026-09-23", "2026-09-23"));
    assert_eq!(r("last Wednesday"), span("2026-09-16", "2026-09-16"));
    assert_eq!(r("last weekend"), span("2026-09-19", "2026-09-20"));
    assert_eq!(r("a while back"), None);
    assert_eq!(r(""), None);
}

#[test]
fn test_resolver_knows_the_rest_of_its_set() {
    let now = at(9, 23, 20, 0); // Wednesday 2026-09-23
    let r = |s: &str| mi::resolve_when(s, now);
    let span = |a: &str, b: &str| Some((d(a), d(b)));
    assert_eq!(
        r("the day before yesterday"),
        span("2026-09-21", "2026-09-21")
    );
    assert_eq!(r("a couple of days ago"), span("2026-09-21", "2026-09-21"));
    assert_eq!(r("past 3 days"), span("2026-09-20", "2026-09-23"));
    assert_eq!(r("last few days"), span("2026-09-20", "2026-09-23"));
    assert_eq!(r("recently"), span("2026-09-16", "2026-09-23"));
    assert_eq!(r("this weekend"), span("2026-09-19", "2026-09-20"));
    assert_eq!(r("mid August"), span("2026-08-11", "2026-08-20"));
    assert_eq!(r("the end of August"), span("2026-08-21", "2026-08-31"));
    assert_eq!(r("3 Sept 2025"), span("2025-09-03", "2025-09-03"));
    assert_eq!(r("two months ago"), span("2026-07-01", "2026-07-31"));
    assert_eq!(r("last year"), span("2025-01-01", "2025-12-31"));
    assert_eq!(r("this year"), span("2026-01-01", "2026-09-23"));
    assert_eq!(r("since last week"), span("2026-09-14", "2026-09-23"));
    assert_eq!(r("Tuesday evening"), span("2026-09-22", "2026-09-22"));
    assert_eq!(r("September 30"), span("2025-09-30", "2025-09-30")); // the latest one on or before today
    assert_eq!(r("2026-02-30"), None);
}

#[test]
fn test_resolve_args_from_alone_both_and_unreadable() {
    let now = at(9, 23, 20, 0);
    assert_eq!(
        mi::resolve_args(Some("last week"), None, now),
        (Some((d("2026-09-14"), d("2026-09-20"))), false)
    );
    assert_eq!(
        mi::resolve_args(Some("2026-09-01"), Some("2026-09-05"), now),
        (Some((d("2026-09-01"), d("2026-09-05"))), false)
    );
    assert_eq!(mi::resolve_args(None, None, now), (None, false));
    assert_eq!(mi::resolve_args(Some("someday"), None, now), (None, true));
    assert_eq!(
        mi::resolve_args(None, Some("2026-09-05"), now),
        (Some((d("2025-09-05"), d("2026-09-05"))), false)
    );
    assert_eq!(
        mi::widen((d("2026-09-14"), d("2026-09-20")), d("2026-09-23")),
        (d("2026-09-07"), d("2026-09-23"))
    );
}

// ---- units ----------------------------------------------------------------------------------------------------------------

fn row(ts: &str, ch: &str, conv: &str, who: &str, kind: &str, text: &str) -> Map<String, Value> {
    json!({"ts": ts, "ch": ch, "conv": conv, "who": who, "kind": kind, "text": text})
        .as_object()
        .unwrap()
        .clone()
}

#[test]
fn test_units_are_rounds_an_owner_line_with_eves_reply() {
    let mut tool = row(
        "2026-09-01T10:02:30",
        "telegram",
        "a",
        "eve",
        "tool",
        "found 3 results",
    );
    tool.insert("tool".into(), json!("web_search"));
    let rows = vec![
        row(
            "2026-09-01T10:00:00",
            "telegram",
            "a",
            "eve",
            "say",
            "Morning.",
        ),
        row("2026-09-01T10:01:00", "telegram", "a", "owner", "say", "Hi"),
        row("2026-09-01T10:02:00", "telegram", "a", "eve", "say", "Hey!"),
        tool,
        row(
            "2026-09-01T10:03:00",
            "telegram",
            "a",
            "owner",
            "say",
            "(forgotten)",
        ),
        row(
            "2026-09-01T10:04:00",
            "telegram",
            "a",
            "system",
            "close",
            "",
        ),
        row(
            "2026-09-01T10:05:00",
            "bus",
            "b",
            "owner",
            "say",
            "Tracker: 5",
        ),
    ];
    let units = mi::units_of("2026-09-01", &rows, &speaker);
    assert_eq!(
        units.iter().map(|u| u.head.as_str()).collect::<Vec<_>>(),
        ["eve: Morning.", "Owner: Hi", "Owner: Tracker: 5"]
    );
    assert_eq!(
        units[1].reply,
        ["eve: Hey!", "eve (web search): found 3 results"]
    );
    assert_eq!(units[1].before, "eve: Morning.");
    assert_eq!(units[2].ch, "bus");
    // the hash is of the model, the width and the body, as the Python keeps it
    let body = "Owner: Hi\neve: Hey!\neve (web search): found 3 results";
    assert_eq!(units[1].body(), body);
    use sha2::Digest;
    let want = hex::encode(sha2::Sha256::digest(
        format!("text-embedding-3-small|512|{body}").as_bytes(),
    ));
    assert_eq!(units[1].hash(), want);
}

#[test]
fn test_terms_and_the_fts_query() {
    assert_eq!(
        mi::terms("What did my landlord say about the RENT, rent?"),
        ["landlord", "rent"]
    );
    assert_eq!(mi::fts_query("landlord name"), "\"landlord\" OR \"name\"");
    assert_eq!(mi::fts_query("the a"), "");
}

// ---- the ranked search -----------------------------------------------------------------------------------------------------

#[test]
fn test_hybrid_finds_what_keyword_and_misses() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    seed_lines(&tr);
    assert!(hits(&tr.search("landlord name")).is_empty()); // the control: keyword AND misses it
    let emb = FakeEmbedder::default();
    let idx = index_for(&tr, &emb);
    assert!(idx.ensure_built());
    let got = tr.search("landlord name");
    assert_eq!(got["ranked"], true);
    assert!(text_of(&hits(&got)[0]).starts_with("Carla, my landlord"));
    assert!(hits(&got)[0]["after"]
        .as_str()
        .unwrap()
        .starts_with("eve: Good to know")); // the reply rides along
                                            // by meaning alone: no word of the query is in the line
    let got = tr.search("bike theft");
    assert!(
        !hits(&got).is_empty() && text_of(&hits(&got)[0]).starts_with("Someone stole my bicycle")
    );
    assert!(hits(&tr.search("today")).len() <= mi::TOP_K);
    idx.close();
}

#[test]
fn test_hybrid_respects_the_range_and_the_char_cap() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    seed_lines(&tr);
    for i in 0..30 {
        say(
            &tr,
            &format!("t-x{i}"),
            "owner",
            &format!("Bike note {i}: {}", "spokes and gears ".repeat(20)),
            at(9, 23, 8, i),
        );
    }
    let emb = FakeEmbedder::default();
    let idx = index_for(&tr, &emb);
    idx.ensure_built();
    let ranged = Search {
        since: "2026-09-19".into(),
        until: "2026-09-19".into(),
        ..Search::default()
    };
    let got = tr.search_with("bicycle", &ranged);
    assert_eq!(
        hits(&got)
            .iter()
            .map(|h| h["day"].clone())
            .collect::<Vec<_>>(),
        ["2026-09-19"]
    );
    let wide = tr.search("bike");
    assert!(py::len(&py::dumps(&wide)) <= 3000 && wide["truncated"] == true);
    idx.close();
}

#[test]
fn test_falls_back_to_keyword_and_when_embeddings_or_index_are_down() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    seed_lines(&tr);
    let keyword = tr.search("landlord");
    let emb = FakeEmbedder::default();
    let idx = index_for(&tr, &emb);
    assert!(idx
        .search("landlord", "2026-09-01", "2026-09-30", mi::TOP_K, None)
        .is_none()); // not built: no ranking
    idx.ensure_built();
    emb.set_fail(true); // the query's vector fails
    let got = tr.search("landlord");
    assert!(got.get("ranked").is_none() && got["hits"] == keyword["hits"]);
    assert!(hits(&tr.search("landlord name")).is_empty()); // and it is the AND, as before
    idx.close();
    assert_eq!(tr.search("landlord")["hits"], keyword["hits"]); // a closed index: the same
}

#[test]
fn test_a_new_line_is_searchable_at_once_and_reaches_the_index_off_the_loop() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    seed_lines(&tr);
    let emb = FakeEmbedder::default();
    let idx = index_for(&tr, &emb);
    {
        let mut t = idx.tuning.write();
        t.debounce_secs = 0.05;
        t.embed_idle_secs = 0.1;
    }
    let notify = idx.clone();
    tr.add_listener(Arc::new(move |day: &str| notify.notify(day)));
    idx.start();
    assert!(wait_until(|| idx.built(), 100, 20));
    say(
        &tr,
        "t-5",
        "owner",
        "The new apartment wifi password is on the fridge.",
        at(9, 24, 19, 0),
    );
    let got = tr.search("wifi password");
    assert_eq!(got["ranked"], true);
    assert!(text_of(&hits(&got)[0]).starts_with("The new apartment wifi"));
    assert!(wait_until(
        || idx.stats().vectors == idx.stats().units,
        100,
        20
    )); // the worker embeds it
    let s = idx.stats();
    assert_eq!((s.vectors, s.units), (5, 5));
    idx.close();
}

#[test]
fn test_a_growing_round_leaves_no_vectors_behind_and_the_worker_embeds_it_once_quiet() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 21, 12, 0));
    say(&tr, "t-1", "owner", "start", at(9, 21, 10, 0));
    let emb = FakeEmbedder::default();
    let idx = index_for(&tr, &emb);
    idx.ensure_built();
    let day = tr.day_of(at(9, 21, 10, 0));
    for i in 0..40 {
        // each refresh embeds the grown round
        say(
            &tr,
            "t-1",
            "eve",
            &format!("reply part {i}"),
            at(9, 21, 10, 1),
        );
        idx.refresh(Some(std::slice::from_ref(&day)), true);
    }
    assert_eq!((idx.stats().units, idx.stats().vectors), (1, 1)); // was 41: one per version of the round
    idx.close();
    // the worker: the words at once, the vector only after EMBED_IDLE_SECS with no new line
    let tr2 = make_store(&d.path().join("w"), at(9, 21, 12, 0));
    say(&tr2, "t-1", "owner", "start", at(9, 21, 10, 0));
    let emb2 = FakeEmbedder::default();
    let idx2 = index_for(&tr2, &emb2);
    {
        let mut t = idx2.tuning.write();
        t.debounce_secs = 0.02;
        t.embed_idle_secs = 0.5;
    }
    let notify = idx2.clone();
    tr2.add_listener(Arc::new(move |day: &str| notify.notify(day)));
    idx2.start();
    assert!(wait_until(|| idx2.built(), 100, 20));
    let first = emb2.texts();
    for i in 0..5 {
        say(
            &tr2,
            "t-1",
            "eve",
            &format!("reply part {i}"),
            at(9, 21, 10, 1),
        );
        std::thread::sleep(StdDuration::from_millis(100));
    }
    assert_eq!(emb2.texts(), first); // still talking: nothing embedded
    wait_until(
        || idx2.stats().vectors == idx2.stats().units && emb2.texts() > first,
        100,
        50,
    );
    assert_eq!(emb2.texts(), first + 1); // one embedding, of the whole round
    assert_eq!(idx2.stats().vectors, 1);
    idx2.close();
}

#[test]
fn test_bm25_candidates_come_from_the_searched_days() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    for i in 0..20 {
        // outside the range, the word many times
        say(
            &tr,
            &format!("t-o{i}"),
            "owner",
            "rent rent rent",
            at(8, 1 + i, 9, 0),
        );
    }
    say(
        &tr,
        "t-in",
        "owner",
        "The rent went up again this month.",
        at(9, 20, 9, 0),
    );
    let emb = FakeEmbedder::default();
    let idx = index_for(&tr, &emb);
    {
        let mut t = idx.tuning.write();
        t.fts_candidates = 5;
        t.min_cos = 2.0; // found by words only: BM25 decides
    }
    idx.ensure_built();
    let got = idx
        .search("rent", "2026-09-20", "2026-09-20", mi::TOP_K, None)
        .unwrap();
    assert_eq!(
        hits(&got)
            .iter()
            .map(|h| h["day"].clone())
            .collect::<Vec<_>>(),
        ["2026-09-20"]
    );
    idx.close();
}

#[test]
fn test_vectors_are_kept_by_text_so_a_rebuild_embeds_nothing_twice() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    seed_lines(&tr);
    let emb = FakeEmbedder::default();
    let idx = index_for(&tr, &emb);
    idx.ensure_built();
    let first = emb.texts();
    // the eval's seed shape
    idx.with_db(|db| {
        db.execute_batch("DELETE FROM fts; DELETE FROM units; DELETE FROM days;")
            .unwrap()
    })
    .unwrap();
    idx.ensure_built();
    assert_eq!(emb.texts(), first);
    assert_eq!(idx.stats().units, 4);
    idx.close();
}

#[test]
fn test_a_channel_narrowed_search_keeps_to_its_channels() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    seed_lines(&tr);
    let emb = FakeEmbedder::default();
    let idx = index_for(&tr, &emb);
    idx.ensure_built();
    let voice = Search {
        channel: "voice".into(),
        ..Search::default()
    };
    let got = tr.search_with("squatted bicycle", &voice);
    assert_eq!(got["ranked"], true);
    assert!(hits(&got).iter().all(|h| h["ch"] == "voice") && !hits(&got).is_empty());
    idx.close();
}

// ---- forget reaches the index ----------------------------------------------------------------------------------------------

fn file_holds(path: &Path, needle: &str) -> bool {
    let bytes = std::fs::read(path).unwrap().to_ascii_lowercase();
    let needle = needle.to_ascii_lowercase().into_bytes();
    bytes.windows(needle.len()).any(|w| w == needle.as_slice())
}

#[test]
fn test_forget_reaches_the_index_rows_and_vectors() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    seed_lines(&tr);
    let emb = FakeEmbedder::default();
    let idx = index_for(&tr, &emb);
    idx.ensure_built();
    say(
        &tr,
        "t-9",
        "owner",
        &format!("My locker code is {SENTINEL}."),
        at(9, 24, 18, 0),
    );
    say(&tr, "t-9", "eve", "Got it.", at(9, 24, 18, 1));
    idx.ensure_built();
    let path = mi::index_path(tr.root.parent().unwrap());
    assert!(file_holds(&path, SENTINEL)); // the positive control
    let pred = match_query(SENTINEL).unwrap();
    assert_eq!(idx.count(&*pred), 1);
    assert_eq!(tr.redact(SENTINEL, false), 2);
    assert_eq!(idx.forget(&*pred).unwrap(), 1);
    assert!(!file_holds(&path, SENTINEL));
    let orphans: i64 = idx
        .with_db(|db| {
            db.query_row(
                "SELECT COUNT(*) FROM vectors WHERE hash NOT IN (SELECT hash FROM units)",
                [],
                |r| r.get(0),
            )
            .unwrap()
        })
        .unwrap();
    assert_eq!(orphans, 0); // no vector outlives its round
    let found = idx
        .search(SENTINEL, "2026-09-01", "2026-09-24", mi::TOP_K, None)
        .unwrap();
    assert!(hits(&found).iter().all(|h| !text_of(h).contains(SENTINEL)));
    let got_it = idx
        .search("Got it", "2026-09-24", "2026-09-24", mi::TOP_K, None)
        .unwrap();
    assert!(hits(&got_it)
        .iter()
        .all(|h| !h["after"].as_str().unwrap().contains("Got it"))); // eve's answer went too
    idx.close();
}

#[test]
fn test_forget_reaches_words_past_the_first_2000_chars_of_a_round() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    seed_lines(&tr);
    let emb = FakeEmbedder::default();
    let idx = index_for(&tr, &emb);
    say(
        &tr,
        "t-9",
        "owner",
        "Tell me a long story please.",
        at(9, 24, 18, 0),
    );
    say(
        &tr,
        "t-9",
        "eve",
        &format!(
            "{} the secret is {SENTINEL}",
            "filler words here ".repeat(130)
        ),
        at(9, 24, 18, 1),
    );
    idx.ensure_built();
    let path = mi::index_path(tr.root.parent().unwrap());
    assert!(file_holds(&path, SENTINEL)); // the positive control: the reply is stored
    let pred = match_query(SENTINEL).unwrap();
    assert_eq!(idx.count(&*pred), 1); // counted past the FTS body's clip
    tr.redact(SENTINEL, false);
    assert_eq!(idx.forget(&*pred).unwrap(), 1);
    assert!(!file_holds(&path, SENTINEL));
    idx.close();
}

#[test]
fn test_forget_cleans_an_index_opened_with_no_embedder() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    seed_lines(&tr);
    let emb = FakeEmbedder::default();
    let on = index_for(&tr, &emb);
    say(
        &tr,
        "t-9",
        "owner",
        &format!("My locker code is {SENTINEL}."),
        at(9, 24, 18, 0),
    );
    on.ensure_built();
    on.close();
    let path = mi::index_path(tr.root.parent().unwrap());
    assert!(file_holds(&path, SENTINEL)); // the positive control: the file holds it
    let source: Arc<dyn mi::DaySource> = tr.clone();
    let off = mi::open_index(tr.root.parent().unwrap(), source, None, false).unwrap(); // the switch is off
    let pred = match_query(SENTINEL).unwrap();
    assert_eq!(off.count(&*pred), 1);
    tr.redact(SENTINEL, false);
    assert_eq!(off.forget(&*pred).unwrap(), 1);
    assert!(!file_holds(&path, SENTINEL));
    off.close();
}

#[test]
fn test_a_forget_that_cannot_clean_the_index_says_so() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    seed_lines(&tr);
    let emb = FakeEmbedder::default();
    let idx = index_for(&tr, &emb);
    idx.tuning.write().compact_waits = vec![0.0, 0.0];
    say(
        &tr,
        "t-9",
        "owner",
        &format!("My locker code is {SENTINEL}."),
        at(9, 24, 18, 0),
    );
    idx.ensure_built();
    idx.compact_fault.store(1, Ordering::SeqCst); // the file stays busy
    let pred = match_query(SENTINEL).unwrap();
    let err = idx.forget(&*pred).unwrap_err();
    assert_eq!(idx.compact_fault.load(Ordering::SeqCst) - 1, 2); // tried again before giving up
    assert_eq!(err.0, "OperationalError");
    idx.close();
}

#[test]
fn test_only_the_daemon_starts_the_index_worker() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    seed_lines(&tr);
    let emb = FakeEmbedder::default();
    let idx = index_for(&tr, &emb);
    idx.ensure_built();
    assert!(!idx.has_worker()); // the CLI and the eval: no worker
                                // Review D10: a search before the index is built must not start the build-and-embed worker either.
    idx.set_built(false); // as for a missing index file
    assert!(idx
        .search("rent", "2026-09-01", "2026-09-24", mi::TOP_K, None)
        .is_none()); // not built: the keyword AND
    assert!(!idx.has_worker());
    idx.close();
    let again = index_for(&tr, &emb);
    again.start();
    assert!(again.has_worker());
    again.close();
}

#[test]
fn test_open_index_follows_the_transcripts_guards() {
    let d = tmp();
    let off = Arc::new(Transcripts::new(
        TranscriptConfig::new(false, d.path().join("mem").join("transcripts")),
        None,
        None,
    ));
    let source: Arc<dyn mi::DaySource> = off;
    assert!(mi::open_index(&d.path().join("mem"), source, None, false).is_none()); // transcripts off: no index
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    let source: Arc<dyn mi::DaySource> = tr.clone();
    let idx = mi::open_index(tr.root.parent().unwrap(), source, None, false).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&idx.path), 0o600);
    assert_eq!(mode(idx.path.parent().unwrap()), 0o700);
    assert!(idx
        .search("anything", "2026-09-01", "2026-09-24", mi::TOP_K, None)
        .is_none()); // no embedder: no ranking
    idx.close();
}

#[test]
fn test_a_file_of_another_model_is_built_again() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    seed_lines(&tr);
    let emb = FakeEmbedder::default();
    let idx = index_for(&tr, &emb);
    idx.ensure_built();
    assert_eq!(idx.stats().units, 4);
    idx.with_db(|db| {
        db.execute("UPDATE meta SET v = 'another-model' WHERE k = 'model'", [])
            .unwrap()
    })
    .unwrap();
    idx.close();
    let again = index_for(&tr, &emb);
    assert_eq!(again.stats(), mi::IndexStats::default());
    let model: String = again
        .with_db(|db| {
            db.query_row("SELECT v FROM meta WHERE k = 'model'", [], |r| r.get(0))
                .unwrap()
        })
        .unwrap();
    assert_eq!(model, mi::EMBED_MODEL);
    again.close();
}

// ---- loose keyword search ---------------------------------------------------------------------------------------------------

#[test]
fn test_loose_search_ranks_the_rarer_word_first() {
    let d = tmp();
    let tr = make_store(d.path(), at(9, 24, 20, 0));
    say(
        &tr,
        "t-r",
        "owner",
        "The studio rent is $1,180 a month.",
        at(9, 2, 9, 0),
    );
    for i in 0..6 {
        say(
            &tr,
            &format!("t-n{i}"),
            "owner",
            &format!("New playlist number {i}."),
            at(9, 10 + i, 9, 0),
        );
    }
    assert!(hits(&tr.search("new rent")).is_empty()); // the control: no line has both
    let got = tr.search_with(
        "new rent",
        &Search {
            loose: true,
            ..Search::default()
        },
    );
    assert!(text_of(&hits(&got)[0]).starts_with("The studio rent")); // older, but the rarer word
    assert_eq!(hits(&got).len(), 7);
}

// ---- the verdict (test_memory_verdict.py) -----------------------------------------------------------------------------------

#[test]
fn test_every_switch_default_equals_the_verdict() {
    let verdict = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../../bridge/tests/fixtures/memory_eval/VERDICT.md"
    ));
    let section = verdict.split_once("## Defaults").unwrap().1;
    let row =
        Regex::new(r"(?m)^\|\s*`?([a-z_]+)`?\s*\|\s*`?(EVE_MEMORY_[A-Z_]+)`?\s*\|\s*(ON|OFF)\s*\|")
            .unwrap();
    let table: HashMap<String, (String, bool)> = row
        .captures_iter(section)
        .map(|c| (c[1].to_string(), (c[2].to_string(), &c[3] == "ON")))
        .collect();
    for ((key, var), (dkey, default)) in mi::SWITCHES.iter().zip(mi::DEFAULTS.iter()) {
        assert_eq!(key, dkey);
        assert_eq!(table.get(*key), Some(&(var.to_string(), *default)), "{key}");
    }
}

#[test]
fn test_an_unset_switch_reads_its_default_and_a_set_one_reads_its_value() {
    for ((key, var), (_, default)) in mi::SWITCHES.iter().zip(mi::DEFAULTS.iter()) {
        let env = |v: &str| HashMap::from([(var.to_string(), v.to_string())]);
        assert_eq!(mi::switch(key, Some(&HashMap::new())), *default);
        assert_eq!(mi::switch(key, Some(&env(""))), *default);
        assert!(mi::switch(key, Some(&env("1"))));
        assert!(!mi::switch(key, Some(&env("0"))));
    }
}

#[test]
fn test_words_and_numbers_are_read_as_python_reads_them() {
    // Python's `\w` takes letters, numbers and `_`, never a combining mark; `\d` and int() take any decimal digit.
    assert_eq!(mi::terms("cafe\u{301} \u{915}\u{93e} x_y"), ["cafe", "x_y"]);
    let now = tz().with_ymd_and_hms(2026, 9, 10, 12, 0, 0).unwrap();
    assert_eq!(
        mi::resolve_when("\u{663} days ago", now),
        Some((d("2026-09-07"), d("2026-09-07")))
    );
    assert_eq!(
        mi::resolve_when("\u{662}\u{660}\u{662}\u{666}-\u{660}\u{669}-\u{660}\u{663}", now),
        Some((d("2026-09-03"), d("2026-09-03")))
    );
    assert_eq!(
        mi::resolve_when("september \u{665}", now),
        Some((d("2026-09-05"), d("2026-09-05")))
    );
}
