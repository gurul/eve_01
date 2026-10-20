//! The chat-core unit: link_chat.py's module functions (the OpenUI guide and strip, the conversation read back from
//! the transcript across the 04:00 day, cursors, the chief's cards for Work, HiddenJobs, the controls listing, one
//! turn's SSE framing) and link_topics.py (topics, titles, bookmarks). Ports bridge/tests/test_link_topics.py and the
//! tests in bridge/tests/test_link_chat.py that call module functions directly; the ones that go through the HTTP door
//! are the chat-door unit's (tests/chat_door.rs).

use std::collections::{BTreeSet, HashMap};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use chrono::{Duration, NaiveDateTime, TimeZone};
use serde_json::{json, Value};

use eve_link::link_chat::{self, HiddenJobs, OPENUI_GONE, OPENUI_PLACEHOLDER};
use eve_link::link_topics::{self, segment, AskFn, Bookmarks, Titler, Topic};
use eve_link::seams::{Chief, Headers, Hub, JobCard, JobPhase, Obj};
use eve_transcripts::{Append, Dt, TranscriptConfig, Transcripts};

// ---- helpers ------------------------------------------------------------------------------------------------------

fn store_at(tmp: &Path) -> Transcripts {
    let store = Transcripts::new(TranscriptConfig::new(true, tmp.join("transcripts")), None, None);
    assert!(store.enabled);
    store
}

/// `datetime.strptime(f"{day} {hhmm}", "%Y-%m-%d %H:%M").astimezone()`.
fn at(day: &str, hhmm: &str) -> Dt {
    let naive = NaiveDateTime::parse_from_str(&format!("{day} {hhmm}"), "%Y-%m-%d %H:%M").unwrap();
    chrono::Local.from_local_datetime(&naive).earliest().unwrap().fixed_offset()
}

/// `store.append(ch, conv, who, kind, text, now=when, via=via)`.
#[allow(clippy::too_many_arguments)]
fn put(store: &Transcripts, ch: &str, conv: &str, who: &str, kind: &str, text: &str, when: Dt, via: &str) {
    let opts = Append { now: Some(when), via: via.to_string(), ..Append::default() };
    assert!(store.append_with(ch, conv, who, kind, text, opts), "{text}");
}

fn say(store: &Transcripts, who: &str, text: &str, when: Dt) {
    put(store, "telegram", "t-1", who, "say", text, when, "");
}

fn texts(rows: &[Obj]) -> Vec<String> {
    rows.iter().map(|r| r["text"].as_str().unwrap().to_string()).collect()
}

fn s<'a>(r: &'a Obj, k: &str) -> &'a str {
    r[k].as_str().unwrap()
}

/// test_link_topics.row: one message as link_chat::row makes it.
fn trow(i: usize, t: f64, role: &str, text: &str) -> Obj {
    let Value::Object(o) = json!({"id": format!("2026-10-13:{i}"), "role": role, "text": text, "t": t}) else {
        unreachable!()
    };
    o
}

fn owner(i: usize, t: f64, text: &str) -> Obj {
    trow(i, t, "owner", text)
}

fn one(rows: &[Obj]) -> Topic {
    let mut got = segment(rows, link_topics::GAP_SECS);
    assert_eq!(got.len(), 1, "{got:?}");
    got.remove(0)
}

fn env(pairs: &[(&str, &str)]) -> eve_link::Env {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

// ---- link_topics: topics ----------------------------------------------------------------------------------------

#[test]
fn a_pause_longer_than_45_minutes_starts_a_new_topic_and_exactly_45_does_not() {
    let gap = link_topics::GAP_SECS;
    let rows = [
        owner(0, 0.0, "plan the trip"),
        trow(1, 60.0, "eve", "Sure: where to?"),
        owner(2, 60.0 + gap, "Lisbon"),             // exactly 45 min: same topic
        owner(3, 60.0 + 2.0 * gap + 1.0, "new thing"), // longer: a new one
    ];
    let got = segment(&rows, gap);
    let [a, b] = got.as_slice() else { panic!("{got:?}") };
    assert_eq!(
        (a.id.as_str(), a.last_id.as_str(), a.count, a.title.as_str()),
        ("2026-10-13:0", "2026-10-13:2", 3, "plan the trip")
    );
    assert_eq!(a.preview, "Sure: where to?");
    assert_eq!((b.id.as_str(), b.count), ("2026-10-13:3", 1));
}

#[test]
fn eves_own_lines_alone_are_not_a_topic() {
    assert!(segment(&[trow(0, 0.0, "eve", "Reminder: water the plants")], link_topics::GAP_SECS).is_empty());
}

#[test]
fn a_hello_is_not_a_title_the_next_message_is() {
    let t = one(&[owner(0, 0.0, "Hi eve!"), trow(1, 5.0, "eve", "Hi!"), owner(2, 30.0, "what's on tomorrow?")]);
    assert_eq!(t.title, "what's on tomorrow?");
    let only = one(&[owner(0, 0.0, "hey"), trow(1, 5.0, "eve", "Hey! What's up?")]);
    assert_eq!(only.title, "hey");
    assert!(!only.important); // nothing else said
}

#[test]
fn friendly_small_talk_is_small_talk() {
    for hello in [
        "It's going amazing, how are you?",
        "What's good, friend?",
        "Hey friend.",
        "How are you bro?",
        "Are you there friend?",
        "How are u",
        "hi eve!",
        "what's up",
        "how's it going",
        "I'm good",
        "thank you buddy",
    ] {
        let t = one(&[
            owner(0, 0.0, hello),
            trow(1, 5.0, "eve", "Hi!"),
            owner(2, 9.0, hello),
            trow(3, 12.0, "eve", "Here."),
        ]);
        assert!(!t.important, "{hello}");
    }
    let t = one(&[owner(0, 0.0, "how are you at fixing bike chains when the link is bent?")]);
    assert!(t.important); // long enough to be a question
    let t = one(&[owner(0, 0.0, "hey how do I fix the bike chain")]);
    assert!(t.important); // a hello that asks something
}

#[test]
fn a_title_says_something() {
    let t = one(&[owner(0, 0.0, "Is flumbo."), trow(1, 5.0, "eve", "Sorry?"), owner(2, 9.0, "Is the garage open today")]);
    assert_eq!(t.title, "Is the garage open today");
}

#[test]
fn only_topics_with_substance_are_important() {
    let important = |rows: &[Obj]| -> Vec<bool> {
        segment(rows, link_topics::GAP_SECS).iter().map(|t| t.important).collect()
    };
    // small talk, even three messages
    assert_eq!(important(&[owner(0, 0.0, "hi"), trow(1, 5.0, "eve", "Hi!"), owner(2, 9.0, "thanks")]), [false]);
    assert_eq!(important(&[owner(0, 0.0, "call mom")]), [false]); // two words, nothing back
    assert_eq!(important(&[owner(0, 0.0, "find me a quiet cafe near the office")]), [true]); // six words or more
    let talk = [owner(0, 0.0, "call mom"), trow(1, 5.0, "eve", "Calling."), owner(2, 9.0, "later"), trow(3, 12.0, "eve", "OK.")];
    assert_eq!(important(&talk), [true]); // it went back and forth
}

#[test]
fn titles_are_plain_and_clipped_at_a_word() {
    let long = format!("**Can you** help me `figure out` {}", "the best way to organise everything ".repeat(4));
    let t = one(&[owner(0, 0.0, &long)]);
    assert!(t.title.starts_with("Can you help me figure out the best"), "{}", t.title);
    assert!(t.title.ends_with('…'));
    assert!(t.title.chars().count() <= link_topics::TITLE_CHARS && !t.title.contains('*') && !t.title.contains('`'));
}

#[test]
fn plain_drops_programs_and_a_clicked_context() {
    // the plain line a card shows: no fence, no component action's context, marks gone, whitespace collapsed
    assert_eq!(link_topics::plain("Here:\n```openui-lang\nroot = Card([])\n```\nmore"), "Here: more");
    assert_eq!(link_topics::plain("Pick one\n\n[\"User clicked: Tea\"]"), "Pick one");
    assert_eq!(link_topics::plain("open fence ```js\nlet x"), "open fence");
    assert_eq!(link_topics::plain("# Head *bold* | cell"), "Head bold cell");
    assert_eq!(link_topics::clip("short", 60), "short");
    assert_eq!(link_topics::clip("abcdefghij", 5), "abcd…"); // no space: cut at the character
    assert_eq!(link_topics::clip("one two, three", 10), "one two…"); // the cut drops its end punctuation
    assert_eq!(link_topics::clip("abc", 0), "ab…"); // Python's text[:-1]
}

// ---- link_topics: bookmarks -------------------------------------------------------------------------------------

#[test]
fn bookmarks_are_private_survive_and_unset() {
    let tmp = tempfile::tempdir().unwrap();
    let b = Bookmarks::new(Some(&tmp.path().join("chat").join("bookmarks.json")));
    b.set("2026-10-13:0", true, Some(10.0)).unwrap();
    b.set("2026-10-13:0", true, Some(99.0)).unwrap(); // saving again keeps the first time
    assert_eq!(b.load().into_iter().collect::<Vec<_>>(), [("2026-10-13:0".to_string(), 10.0)]);
    assert_eq!(mode(&b.path), 0o600);
    assert_eq!(mode(b.path.parent().unwrap()), 0o700);
    b.set("2026-10-13:0", false, None).unwrap();
    assert!(b.load().is_empty());
    std::fs::write(&b.path, "not json").unwrap();
    assert!(b.load().is_empty()); // a damaged file is no bookmarks
}

#[test]
fn bookmarks_read_what_python_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("bm.json");
    // json.dump({"version": 1, "items": {...}}): an int time, a bool (an int in Python), and ids that are not ids
    std::fs::write(
        &path,
        r#"{"version": 1, "items": {"2026-10-13:4": 1760000000.5, "2026-10-12:0": 7, "2026-10-11:1": true, "../x": 1, "2026-10-10:2": "no"}}"#,
    )
    .unwrap();
    let got: Vec<(String, f64)> = Bookmarks::new(Some(&path)).load().into_iter().collect();
    assert_eq!(
        got,
        [
            ("2026-10-13:4".to_string(), 1760000000.5),
            ("2026-10-12:0".to_string(), 7.0),
            ("2026-10-11:1".to_string(), 1.0)
        ]
    );
    let b = Bookmarks::new(Some(&path));
    b.set("2026-10-09:0", true, Some(5.0)).unwrap();
    let raw = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        raw,
        r#"{"version": 1, "items": {"2026-10-13:4": 1760000000.5, "2026-10-12:0": 7.0, "2026-10-11:1": 1.0, "2026-10-09:0": 5.0}}"#
    );
}

// ---- link_topics: the view (test_routes_list_save_and_refuse, the parts below the HTTP door) ---------------------

#[test]
fn the_view_lists_important_topics_newest_first_and_the_saved_ones() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    let day = "2026-10-13";
    say(&store, "owner", "plan the Lisbon trip for early November", at(day, "09:00"));
    say(&store, "eve", "Three days works well.", at(day, "09:01"));
    say(&store, "owner", "hey", at(day, "12:00")); // small talk: no card
    say(&store, "eve", "Hey!", at(day, "12:00"));
    say(&store, "owner", "what's on my calendar for tomorrow?", at(day, "14:00"));
    say(&store, "eve", "Two meetings.", at(day, "14:00"));

    let bm = Bookmarks::new(Some(&tmp.path().join("bm.json")));
    let titler = Arc::new(Titler::new(Some(&tmp.path().join("titles.json")), Some(env(&[("EVE_TOPIC_TITLES", "0")])), None));
    let view = link_topics::view(&store, &bm, link_topics::DEFAULT_LIMIT, Some(&titler));
    let titles = |v: &Value| -> Vec<String> {
        v.as_array().unwrap().iter().map(|t| t["title"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(
        titles(&view["topics"]),
        ["what's on my calendar for tomorrow?", "plan the Lisbon trip for early November"]
    );
    let first = view["topics"][1].as_object().unwrap().clone();
    let keys: Vec<&str> = first.keys().map(String::as_str).collect();
    assert_eq!(
        keys,
        ["id", "first_id", "last_id", "started_at", "ended_at", "count", "title", "preview", "bookmarked"]
    );
    assert_eq!(first["bookmarked"], json!(false));
    assert_eq!(view["bookmarked"], json!([]));
    assert_eq!(first["preview"], json!("Three days works well."));
    assert!(titler.thread.lock().is_none()); // titles off: no pass

    let id = s(&first, "id").to_string();
    assert!(link_topics::known(&store, &id));
    assert!(!link_topics::known(&store, "2026-01-01:0"));
    bm.set(&id, true, None).unwrap();
    let view = link_topics::view(&store, &bm, link_topics::DEFAULT_LIMIT, Some(&titler));
    assert_eq!(titles(&view["bookmarked"]), ["plan the Lisbon trip for early November"]);
    assert_eq!(view["topics"][1]["bookmarked"], json!(true));
    assert_eq!(link_topics::view(&store, &bm, 1, None)["topics"].as_array().unwrap().len(), 1);
}

#[test]
fn a_saved_small_talk_topic_still_shows_and_newest_bookmark_comes_first() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    say(&store, "owner", "hey", at("2026-10-13", "08:00")); // small talk, but saved
    say(&store, "owner", "find me a quiet cafe near the office", at("2026-10-13", "12:00"));
    let bm = Bookmarks::new(Some(&tmp.path().join("bm.json")));
    bm.set("2026-10-13:0", true, Some(5.0)).unwrap();
    bm.set("2026-10-13:1", true, Some(9.0)).unwrap();
    bm.set("2026-10-01:0", true, Some(20.0)).unwrap(); // a topic the days no longer have: drops out
    let view = link_topics::view(&store, &bm, 30, None);
    let ids = |v: &Value| -> Vec<String> {
        v.as_array().unwrap().iter().map(|t| t["id"].as_str().unwrap().to_string()).collect()
    };
    assert_eq!(ids(&view["topics"]), ["2026-10-13:1", "2026-10-13:0"]);
    assert_eq!(ids(&view["bookmarked"]), ["2026-10-13:1", "2026-10-13:0"]);
}

// ---- link_topics: model titles ----------------------------------------------------------------------------------

#[test]
fn model_titles_are_cached_by_count_and_small_talk_hides_a_card() {
    let tmp = tempfile::tempdir().unwrap();
    let asked: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = asked.clone();
    let ask: AskFn = Arc::new(move |system: &str, text: &str| {
        assert_eq!(system, link_topics::TITLE_SYSTEM);
        log.lock().unwrap().push(text.to_string());
        Ok(if text.contains("Lisbon") {
            json!({"title": "Lisbon trip in November.", "small_talk": false})
        } else {
            json!({"title": "Checking in", "small_talk": true})
        })
    });
    let titler = Titler::new(Some(&tmp.path().join("chat").join("titles.json")), Some(env(&[])), Some(ask));
    let mut topics = segment(
        &[
            owner(0, 0.0, "can you plan a three day Lisbon trip"),
            trow(1, 5.0, "eve", "Sure."),
            owner(2, 5000.0, "are you around this evening or busy with things"),
            trow(3, 5005.0, "eve", "Here."),
        ],
        link_topics::GAP_SECS,
    );
    assert_eq!(titler.title_pass(&topics), 2);
    {
        let asked = asked.lock().unwrap();
        assert_eq!(asked.len(), 2);
        assert!(asked[0].contains("Person: can you plan a three day Lisbon trip") || asked[1].contains("Person: can you plan"));
        assert!(asked.iter().any(|a| a == "Person: can you plan a three day Lisbon trip\neve: Sure."));
    }
    assert_eq!(mode(&titler.path), 0o600);
    titler.apply(&mut topics);
    let by_id: HashMap<&str, &Topic> = topics.iter().map(|t| (t.id.as_str(), t)).collect();
    assert_eq!(by_id["2026-10-13:0"].title, "Lisbon trip in November");
    assert!(by_id["2026-10-13:0"].important);
    assert!(!by_id["2026-10-13:2"].important); // the model called it small talk
    assert_eq!(titler.title_pass(&topics), 0); // cached: no second call
    assert_eq!(asked.lock().unwrap().len(), 2);
    let grown = segment(
        &[
            owner(0, 0.0, "can you plan a three day Lisbon trip"),
            trow(1, 5.0, "eve", "Sure."),
            owner(9, 60.0, "add Porto for one night"),
        ],
        link_topics::GAP_SECS,
    );
    assert_eq!(titler.title_pass(&grown), 1); // it grew: titled again
    assert_eq!(asked.lock().unwrap().len(), 3);
    // the cache as Python writes it: {topic id: {"n", "title", "small_talk"}}
    let cache: Value = serde_json::from_str(&std::fs::read_to_string(&titler.path).unwrap()).unwrap();
    assert_eq!(cache["2026-10-13:0"], json!({"n": 3, "title": "Lisbon trip in November", "small_talk": false}));
    assert_eq!(cache["2026-10-13:2"], json!({"n": 2, "title": "Checking in", "small_talk": true}));
}

#[test]
fn a_failing_model_keeps_the_first_message_title() {
    let tmp = tempfile::tempdir().unwrap();
    let ask: AskFn = Arc::new(|_: &str, _: &str| Err(anyhow::anyhow!("TimeoutError")));
    let titler = Titler::new(Some(&tmp.path().join("titles.json")), Some(env(&[])), Some(ask));
    let mut topics = segment(&[owner(0, 0.0, "can you plan a three day Lisbon trip")], link_topics::GAP_SECS);
    assert_eq!(titler.title_pass(&topics), 0);
    assert!(!titler.path.exists());
    titler.apply(&mut topics);
    assert_eq!(topics[0].title, "can you plan a three day Lisbon trip");
}

#[test]
fn titles_off_never_ask() {
    let tmp = tempfile::tempdir().unwrap();
    let ask: AskFn = Arc::new(|_: &str, _: &str| Ok(json!({"title": "x"})));
    let titler = Arc::new(Titler::new(
        Some(&tmp.path().join("titles.json")),
        Some(env(&[("EVE_TOPIC_TITLES", "0")])),
        Some(ask),
    ));
    assert!(!titler.kick(segment(&[owner(0, 0.0, "can you plan a three day Lisbon trip")], link_topics::GAP_SECS)));
    // and without a test's model, no OpenRouter key means no pass either
    let keyless = Arc::new(Titler::new(Some(&tmp.path().join("t2.json")), Some(env(&[])), None));
    assert!(keyless.enabled && !keyless.kick(Vec::new()));
}

#[test]
fn the_title_model_and_switch_come_from_the_environment() {
    let t = Titler::new(None, Some(env(&[])), None);
    assert_eq!((t.model.as_str(), t.enabled), (link_topics::TITLE_MODEL, true));
    assert!(t.path.ends_with(".config/eve-bridge/chat/topic-titles.json"));
    let t = Titler::new(None, Some(env(&[("EVE_TOPIC_TITLE_MODEL", "  x/y  "), ("EVE_TOPIC_TITLES", " OFF ")])), None);
    assert_eq!((t.model.as_str(), t.enabled), ("x/y", false));
    let t = Titler::new(None, Some(env(&[("EVE_TOPIC_TITLE_MODEL", "   "), ("EVE_TOPIC_TITLES", "yes")])), None);
    assert_eq!((t.model.as_str(), t.enabled), (link_topics::TITLE_MODEL, true));
}

#[test]
fn a_list_titles_in_the_background_without_racing_the_reply() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    say(&store, "owner", "plan the Lisbon trip for early November", at("2026-10-13", "09:00"));
    let ask: AskFn = Arc::new(|_: &str, _: &str| Ok(json!({"title": "Lisbon trip", "small_talk": false})));
    let titler = Arc::new(Titler::new(Some(&tmp.path().join("titles.json")), Some(env(&[])), Some(ask)));
    let bm = Bookmarks::new(Some(&tmp.path().join("bm.json")));
    let first = link_topics::view(&store, &bm, link_topics::DEFAULT_LIMIT, Some(&titler));
    assert!(first["topics"][0].get("_lines").is_none());
    let handle = titler.thread.lock().take().expect("a pass started");
    handle.join().unwrap();
    let again = link_topics::view(&store, &bm, link_topics::DEFAULT_LIMIT, Some(&titler));
    assert_eq!(again["topics"][0]["title"], json!("Lisbon trip"));
    let last = titler.thread.lock().take();
    if let Some(h) = last {
        h.join().unwrap();
    }
}

// ---- infinite scroll: ?before=<message id> ------------------------------------------------------------------------

#[test]
fn older_pages_reach_the_first_day_in_order() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    for d in 1..6 {
        // five days, 60 lines each: 300 lines
        for k in 0..60 {
            let who = if k % 2 == 0 { "owner" } else { "eve" };
            say(&store, who, &format!("day {d} line {k}"), at(&format!("2026-10-0{d}"), &format!("{}:{:02}", 10 + k / 60, k % 60)));
        }
    }
    let (first, cursor) = link_chat::read_messages(&store, None).unwrap();
    assert_eq!(first.len(), link_chat::HISTORY_LIMIT);
    assert_eq!(s(first.last().unwrap(), "text"), "day 5 line 59"); // beyond the last 3 days rule
    assert!(cursor.starts_with("2026-10-05:"));
    let mut seen = first.clone();
    let mut before = s(&first[0], "id").to_string();
    let mut more = true;
    while more {
        let (page, m) = link_chat::read_before(&store, Some(&before), link_chat::HISTORY_LIMIT).unwrap();
        more = m;
        assert!(!page.is_empty());
        assert!(page.last().unwrap()["t"].as_f64() <= seen[0]["t"].as_f64());
        before = s(&page[0], "id").to_string();
        seen = page.into_iter().chain(seen).collect();
    }
    assert_eq!(seen.len(), 300);
    assert_eq!(s(&seen[0], "text"), "day 1 line 0");
    let ids: Vec<(String, i64)> = seen
        .iter()
        .map(|r| {
            let (d, i) = s(r, "id").split_once(':').unwrap();
            (d.to_string(), i.parse().unwrap())
        })
        .collect();
    let mut sorted = ids.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(ids, sorted);
}

#[test]
fn an_older_page_and_a_bad_message_id() {
    // test_the_older_route_answers_a_page_and_refuses_a_bad_id, below the HTTP door
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    for k in 0..5 {
        say(&store, "owner", &format!("line {k}"), at("2026-10-13", &format!("09:0{k}")));
    }
    let (page, more) = link_chat::read_before(&store, Some("2026-10-13:3"), link_chat::HISTORY_LIMIT).unwrap();
    assert_eq!(texts(&page), ["line 0", "line 1", "line 2"]);
    assert!(!more);
    let (page, more) = link_chat::read_before(&store, Some("2026-10-13:3"), 2).unwrap();
    assert_eq!((texts(&page), more), (vec!["line 1".to_string(), "line 2".to_string()], true));
    for bad in ["x", "2026-10-13:-1", "../a"] {
        assert_eq!(
            link_chat::read_before(&store, Some(bad), link_chat::HISTORY_LIMIT).unwrap_err().0,
            "bad message id",
            "{bad}"
        );
    }
}

// ---- link_chat: the conversation read back ----------------------------------------------------------------------

#[test]
fn history_is_oldest_first_and_the_cursor_crosses_the_four_oclock_day() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    say(&store, "owner", "late at night", at("2026-10-08", "03:59"));
    put(&store, "telegram", "t-1", "system", "close", "", at("2026-10-08", "03:59"), "");
    put(&store, "voice", "v-1", "eve", "say", "good morning", at("2026-10-08", "04:01"), "");
    assert_eq!(store.days(), ["2026-10-07", "2026-10-08"]); // 03:59 belongs to the evening before
    let (rows, cursor) = link_chat::read_messages(&store, None).unwrap();
    assert_eq!(texts(&rows), ["late at night", "good morning"]);
    let rvk: Vec<(&str, &str, &str)> = rows.iter().map(|r| (s(r, "role"), s(r, "via"), s(r, "kind"))).collect();
    assert_eq!(rvk, [("owner", "telegram", "say"), ("eve", "voice", "say")]);
    assert_eq!((s(&rows[0], "id"), s(&rows[1], "id"), cursor.as_str()), ("2026-10-07:0", "2026-10-08:0", "2026-10-08:0"));
    assert!((rows[0]["t"].as_f64().unwrap() - at("2026-10-08", "03:59").timestamp() as f64).abs() < 1.0);
    let (after_first, cur) = link_chat::read_messages(&store, Some("2026-10-07:0")).unwrap();
    assert_eq!((texts(&after_first), cur.as_str()), (vec!["good morning".to_string()], "2026-10-08:0"));
    assert_eq!(link_chat::read_messages(&store, Some(&cur)).unwrap(), (vec![], cur.clone())); // nothing new
    put(&store, "telegram", "t-2", "owner", "say", "and again", at("2026-10-08", "09:00"), "");
    let (newer, cur2) = link_chat::read_messages(&store, Some(&cur)).unwrap();
    assert_eq!((texts(&newer), cur2.as_str()), (vec!["and again".to_string()], "2026-10-08:1"));
    assert_eq!(link_chat::read_messages(&store, Some("nope")).unwrap_err().0, "bad cursor");
}

#[test]
fn history_is_the_newest_hundred_lines_however_far_back() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    for (n, day) in ["2026-10-01", "2026-10-02", "2026-10-03", "2026-10-04"].iter().enumerate() {
        put(&store, "telegram", "t", "owner", "say", &format!("day {n}"), at(day, "12:00"), "");
    }
    // every day, not only the last three (owner, 2026-10-13: "keep all chat history, i want infinite scroll")
    assert_eq!(texts(&link_chat::read_messages(&store, None).unwrap().0), ["day 0", "day 1", "day 2", "day 3"]);
    for i in 0..250 {
        put(&store, "telegram", "t", "eve", "say", &format!("line {i}"), at("2026-10-04", "13:00"), "");
    }
    let (rows, cursor) = link_chat::read_messages(&store, None).unwrap();
    assert_eq!(rows.len(), link_chat::HISTORY_LIMIT);
    assert_eq!((s(rows.last().unwrap(), "text"), cursor.as_str()), ("line 249", "2026-10-04:250"));
    let (page, cur) = link_chat::read_messages(&store, Some("2026-10-01:-1")).unwrap();
    assert_eq!(page.len(), link_chat::PAGE_LIMIT); // a long gap comes in pages
    assert_eq!(s(&page[0], "text"), "day 0");
    assert_eq!(cur, s(page.last().unwrap(), "id"));
    let (rest, end) = link_chat::read_messages(&store, Some(&cur)).unwrap();
    assert_eq!(page.len() + rest.len(), 254);
    assert_eq!((s(rest.last().unwrap(), "text"), end), ("line 249", cursor));
}

#[test]
fn an_empty_store_starts_the_cursor_at_today() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    let (rows, cursor) = link_chat::read_messages(&store, None).unwrap();
    assert!(rows.is_empty());
    assert_eq!(cursor, format!("{}:-1", store.day_of(store.now())));
}

#[test]
fn only_what_was_said_is_shown_and_forgotten_lines_are_left_out() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    let when = at("2026-10-08", "10:00");
    put(&store, "telegram", "t", "owner", "say", "my secret plan", when, "");
    put(&store, "telegram", "t", "owner", "say", "keep this", when, "");
    put(&store, "telegram", "t", "owner", "command", "stealth on", when, "");
    put(&store, "telegram", "t", "claude", "relay", "Claude did a thing", when, "");
    put(&store, "bus", "b", "system", "say", "a program's line", when, "");
    let tool = Append { now: Some(when), tool: "web_search".to_string(), ..Append::default() };
    assert!(store.append_with("telegram", "t", "eve", "tool", "searched", tool));
    put(&store, "telegram", "t", "owner", "image", "a photo", when, "");
    put(&store, "telegram", "t", "eve", "say", "from the app", when, "app");
    assert_eq!(store.redact("secret plan", false), 1);
    let (rows, cursor) = link_chat::read_messages(&store, None).unwrap();
    let tkv: Vec<(&str, &str, &str)> = rows.iter().map(|r| (s(r, "text"), s(r, "kind"), s(r, "via"))).collect();
    assert_eq!(
        tkv,
        [
            ("keep this", "say", "telegram"),
            ("Claude did a thing", "say", "other"),
            ("searched", "tool", "telegram"),
            ("a photo", "image", "telegram"),
            ("from the app", "say", "app")
        ]
    );
    let ids: Vec<&str> = rows.iter().map(|r| s(r, "id")).collect();
    assert_eq!(ids, ["2026-10-08:1", "2026-10-08:3", "2026-10-08:5", "2026-10-08:6", "2026-10-08:7"]);
    assert_eq!(cursor, "2026-10-08:7");
}

#[test]
fn claude_on_shows_both_sides_of_the_relay_in_the_app() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    let when = at("2026-10-08", "20:24");
    put(&store, "telegram", "t", "owner", "command", "Claude on", when, "app");
    put(&store, "telegram", "t", "owner", "relay", "What are the capabilities of openui here", when, "app");
    put(&store, "telegram", "t", "claude", "relay", "OpenUI draws the chat's replies.", when, "");
    let (rows, _) = link_chat::read_messages(&store, None).unwrap();
    let rtk: Vec<(&str, &str, &str)> = rows.iter().map(|r| (s(r, "role"), s(r, "text"), s(r, "kind"))).collect();
    assert_eq!(
        rtk,
        [("owner", "What are the capabilities of openui here", "say"), ("eve", "OpenUI draws the chat's replies.", "say")]
    );
    assert_eq!((s(&rows[0], "via"), s(&rows[1], "via")), ("app", "other"));
}

#[test]
fn a_line_without_via_still_loads_and_via_is_written_only_when_given() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    put(&store, "telegram", "t", "owner", "say", "old style", at("2026-10-08", "10:00"), "");
    put(&store, "telegram", "t", "owner", "say", "new style", at("2026-10-08", "10:00"), "app");
    let raw: Vec<Value> = std::fs::read_to_string(store.path_of("2026-10-08"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(raw[0].get("via").is_none() && raw[1]["via"] == json!("app"));
    let rows = link_chat::read_messages(&store, None).unwrap().0;
    assert_eq!(texts(&rows), ["old style", "new style"]);
    assert_eq!((s(&rows[0], "via"), s(&rows[1], "via")), ("telegram", "app"));
}

#[test]
fn days_cross_at_four_even_with_a_clock_near_midnight() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    let base = at("2026-10-08", "00:30");
    for (i, minutes) in [0, 200, 220].iter().enumerate() {
        // 00:30, 03:50, 04:10
        put(&store, "telegram", "t", "owner", "say", &format!("m{i}"), base + Duration::minutes(*minutes), "");
    }
    let (rows, _) = link_chat::read_messages(&store, None).unwrap();
    let it: Vec<(&str, &str)> = rows.iter().map(|r| (s(r, "id"), s(r, "text"))).collect();
    assert_eq!(it, [("2026-10-07:0", "m0"), ("2026-10-07:1", "m1"), ("2026-10-08:0", "m2")]);
}

#[test]
fn an_owner_line_shows_its_attachments_its_reaction_key_and_eves_sources() {
    let tmp = tempfile::tempdir().unwrap();
    let store = store_at(tmp.path());
    let id = "0123456789abcdef0123456789abcdef";
    let when = at("2026-10-09", "10:00");
    let att = vec![
        json!({"id": id, "name": "IMG_1.jpg", "kind": "image"}),
        json!({"id": "not-an-id", "name": "x", "kind": "image"}),
        json!({"id": id, "name": "y", "kind": "exe"}),
    ];
    let opts = Append { now: Some(when), via: "app".into(), att: Some(att), mid: "client-0001".into(), ..Append::default() };
    assert!(store.append_with("telegram", "t", "owner", "say", "look at this [image attached]", opts));
    let src = json!([{"kind": "fact", "text": "Owner likes tea"}, {"kind": "made-up", "text": "x"}]);
    let opts = Append { now: Some(when), via: "app".into(), src: Some(src), ..Append::default() };
    assert!(store.append_with("telegram", "t", "eve", "say", "Nice.", opts));
    let opts = Append { now: Some(when), mid: "short".into(), ..Append::default() };
    assert!(store.append_with("telegram", "t", "owner", "say", "no rid [image attached]", opts));
    let (rows, _) = link_chat::read_messages(&store, None).unwrap();
    // the note goes only when the line carries attachments the app shows itself
    assert_eq!(texts(&rows), ["look at this", "Nice.", "no rid [image attached]"]);
    assert_eq!(rows[0]["rid"], json!("client-0001"));
    assert_eq!(rows[0]["attachments"], json!([{"id": id, "name": "IMG_1.jpg", "kind": "image"}]));
    let keys: Vec<&str> = rows[0].keys().map(String::as_str).collect();
    assert_eq!(keys, ["id", "role", "text", "t", "via", "kind", "rid", "attachments"]);
    assert_eq!(rows[1]["sources"], json!([{"kind": "fact", "text": "Owner likes tea"}]));
    assert!(rows[2].get("rid").is_none() && rows[2].get("attachments").is_none());
}

#[test]
fn a_line_the_app_cannot_place_is_not_shown() {
    let line = |v: Value| -> eve_transcripts::Line {
        let Value::Object(o) = v else { unreachable!() };
        o
    };
    let good = json!({"ts": "2026-10-08T10:00:00+00:00", "ch": "telegram", "who": "owner", "kind": "say", "text": "hi"});
    let row = link_chat::row("2026-10-08", 4, &line(good.clone())).unwrap();
    assert_eq!(row["t"], json!(1791453600.0));
    assert_eq!((s(&row, "id"), s(&row, "via")), ("2026-10-08:4", "telegram"));
    for (k, v) in [
        ("ts", json!("yesterday")),
        ("ts", Value::Null),
        ("ch", json!("bus")),
        ("who", json!("system")),
        ("kind", json!("command")),
        ("text", json!("(forgotten)")),
    ] {
        let mut l = line(good.clone());
        l.insert(k.to_string(), v.clone());
        assert!(link_chat::row("2026-10-08", 0, &l).is_none(), "{k}={v}");
    }
    let mut l = line(good.clone());
    l.remove("ts");
    assert!(link_chat::row("2026-10-08", 0, &l).is_none());
    // a via the app does not know falls back to the channel's; a missing text is empty
    let mut l = line(good);
    l.insert("via".into(), json!("carrier pigeon"));
    l.insert("ch".into(), json!("voice"));
    l.remove("text");
    let row = link_chat::row("2026-10-08", 0, &l).unwrap();
    assert_eq!((s(&row, "via"), s(&row, "text")), ("voice", ""));
}

#[test]
fn att_record_keeps_id_name_and_kind() {
    let u = eve_link_uploads::Upload {
        id: "0123456789abcdef0123456789abcdef".into(),
        kind: "pdf".into(),
        name: "a.pdf".into(),
        mime: "application/pdf".into(),
        size: 9,
        path: "/tmp/x".into(),
    };
    assert_eq!(
        link_chat::att_record(&[u]),
        [json!({"id": "0123456789abcdef0123456789abcdef", "name": "a.pdf", "kind": "pdf"})]
    );
    let many: Vec<Value> =
        (0..6).map(|_| json!({"id": "0123456789abcdef0123456789abcdef", "name": "n", "kind": "text"})).collect();
    assert_eq!(link_chat::att(Some(&Value::Array(many))).len(), link_chat::MAX_ATTACHMENTS);
    assert!(link_chat::att(Some(&json!("x"))).is_empty() && link_chat::att(None).is_empty());
    let long = "é".repeat(200);
    let got = link_chat::att(Some(&json!([{"id": "0123456789abcdef0123456789abcdef", "name": long, "kind": "doc"}])));
    assert_eq!(got[0]["name"].as_str().unwrap().chars().count(), eve_link_uploads::MAX_NAME_CHARS);
}

// ---- link_chat: cursors, ids, the stream ------------------------------------------------------------------------

#[test]
fn a_cursor_splits_into_its_position_and_reaction_version() {
    assert_eq!(link_chat::split_cursor(None).unwrap(), (None, None));
    assert_eq!(link_chat::split_cursor(Some("2026-10-08:3")).unwrap(), (Some("2026-10-08:3".into()), None));
    assert_eq!(link_chat::split_cursor(Some("2026-10-08:-1~42")).unwrap(), (Some("2026-10-08:-1".into()), Some(42)));
    for bad in ["x", "2026-10-08:3~", "2026-10-08:3~x", "2026-10-08:3~1~2", "~1", "2026-10-08:3~12345678901234567"] {
        assert_eq!(link_chat::split_cursor(Some(bad)).unwrap_err().0, "bad cursor", "{bad}");
    }
}

#[test]
fn a_turn_id_is_the_client_id_when_plain_and_free() {
    let none = |_: &str| false;
    assert_eq!(link_chat::turn_id("client-0001", &none), "client-0001");
    let taken = |c: &str| c == "client-0001";
    for other in [link_chat::turn_id("client-0001", &taken), link_chat::turn_id("short", &none), link_chat::turn_id("", &none)] {
        assert_eq!(other.len(), 16);
        assert!(other.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()), "{other}");
    }
    assert_ne!(link_chat::turn_id("", &none), link_chat::turn_id("", &none));
}

#[test]
fn the_content_length_is_read_as_python_reads_it() {
    let h = |v: Option<&str>| -> Headers {
        v.map(|v| HashMap::from([("content-length".to_string(), v.to_string())])).unwrap_or_default()
    };
    assert_eq!(link_chat::length(&h(None)), 0);
    assert_eq!(link_chat::length(&h(Some(""))), 0);
    assert_eq!(link_chat::length(&h(Some(" 12 "))), 12);
    assert_eq!(link_chat::length(&h(Some("1_000"))), 1000);
    assert_eq!(link_chat::length(&h(Some("+7"))), 7);
    assert_eq!(link_chat::length(&h(Some("-3"))), -3);
    for bad in ["x", "1.5", "1__0", "_1", "0x10"] {
        assert_eq!(link_chat::length(&h(Some(bad))), -1, "{bad}");
    }
}

#[test]
fn a_stream_chunk_is_an_openai_delta_framed_as_sse() {
    assert_eq!(
        link_chat::sse("Hi \"you\" é\n"),
        b"data: {\"choices\": [{\"index\": 0, \"delta\": {\"content\": \"Hi \\\"you\\\" \\u00e9\\n\"}}]}\n\n".to_vec()
    );
    let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
    let mut out: Vec<u8> = Vec::new();
    rt.block_on(async {
        let w: &mut eve_link::seams::DynWrite = &mut out;
        link_chat::chunk(w, link_chat::DONE).await.unwrap();
    });
    assert_eq!(out, b"e\r\ndata: [DONE]\n\n\r\n".to_vec());
}

// ---- link_chat: OpenUI ------------------------------------------------------------------------------------------

const DINNERS: &str = r#"Three quick ones.
```openui-lang
root = Card([header, list, choose])
header = CardHeader("Three quick dinners", "Under 30 minutes")
list = ListBlock([d1, d2, d3], "number")
d1 = ListItem("Egg fried rice", "Leftover rice, eggs, frozen peas")
d2 = ListItem("Pasta aglio e olio", "Spaghetti, garlic, chili, olive oil")
d3 = ListItem("Black bean tacos", "Tortillas, beans, salsa, cheese")
choose = Buttons([b1, b2, b3], "row")
b1 = Button("Fried rice", Action([@ToAssistant("Give me the fried rice recipe")]), "secondary")
b2 = Button("Pasta", Action([@ToAssistant("Give me the pasta recipe")]), "secondary")
b3 = Button("Tacos", Action([@ToAssistant("Give me the taco recipe")]), "secondary")
```"#;

#[test]
fn openui_programs_are_one_plain_line_outside_the_app() {
    let p = OPENUI_PLACEHOLDER;
    let fenced = "Here it is.\n\n```openui-lang\nroot = Card([t])\nt = Table([])\n```\n\nAnything else?";
    assert_eq!(link_chat::strip_openui(fenced), format!("Here it is.\n\n{p}\n\nAnything else?"));
    assert_eq!(link_chat::strip_openui("root = Card([])\n"), p); // a bare program
    assert_eq!(link_chat::strip_openui("Look:\n```openui-lang\nroot = Card([])"), format!("Look:\n{p}")); // unclosed
    let plain = "A list:\n\n- one\n- two\n\n```python\nprint(1)\n```";
    assert_eq!(link_chat::strip_openui(plain), plain); // markdown and code untouched
}

#[test]
fn history_text_per_channel() {
    let (p, gone) = (OPENUI_PLACEHOLDER, OPENUI_GONE);
    assert_eq!(link_chat::history_text(DINNERS, true), DINNERS);
    assert_eq!(link_chat::history_text(DINNERS, false), format!("Three quick ones.\n{p}"));
    // a line logged before app history kept blocks: an app turn is not told to "open Eve Link"
    assert_eq!(link_chat::history_text(&format!("Your week:\n\n{p}"), true), format!("Your week:\n\n{gone}"));
    assert_eq!(link_chat::history_text(&format!("Your week:\n\n{p}"), false), format!("Your week:\n\n{p}"));
    assert_eq!(link_chat::history_text("Hi.", true), "Hi.");
    assert_eq!(link_chat::history_text("Hi.", false), "Hi.");
}

#[test]
fn a_bare_program_after_prose_is_stripped_like_the_app_draws_it() {
    // replyParts.ts draws `root = Card(` at any line start (no ``` before it); history must strip it the same way.
    let text = "Here you go:\nroot = Card([t])\nt = TextContent(\"hi\")";
    assert_eq!(link_chat::strip_openui(text), format!("Here you go:\n{OPENUI_PLACEHOLDER}"));
    assert_eq!(link_chat::strip_openui("root = 5 is not a program"), "root = 5 is not a program");
    assert!(link_chat::strip_openui("prose\nroot = Card([x])").ends_with(OPENUI_PLACEHOLDER));
}

/// Collects what `tracing` writes while `f` runs on this thread.
fn logged(f: impl FnOnce()) -> String {
    #[derive(Clone)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf(Arc::default());
    let writer = buf.clone();
    let sub = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .finish();
    tracing::subscriber::with_default(sub, f);
    let out = buf.0.lock().unwrap().clone();
    String::from_utf8(out).unwrap()
}

#[test]
fn no_guide_file_means_no_addition_logged_once() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("absent.txt");
    let log = logged(|| {
        assert_eq!(link_chat::app_block(Some(&path)), "");
        assert_eq!(link_chat::app_block(Some(&path)), "");
    });
    assert_eq!(log.matches("absent.txt").count(), 1, "{log}");
    std::fs::write(&path, "   \n").unwrap();
    assert_eq!(link_chat::app_block(Some(&path)), ""); // empty counts as absent
    std::fs::write(&path, "GUIDE").unwrap();
    // a new file is picked up
    assert_eq!(link_chat::app_block(Some(&path)), format!("{}\n\nGUIDE\n\n{}", *link_chat::APP_RULE, link_chat::APP_LINE));
}

#[test]
fn the_openui_guide_is_rule_reference_line_in_that_order() {
    let tmp = tempfile::tempdir().unwrap();
    let guide = tmp.path().join("openui_prompt.txt");
    std::fs::write(&guide, "OPENUI LANG GUIDE: Table, Chart, Steps.\n").unwrap();
    let block = link_chat::app_block(Some(&guide));
    assert_eq!(
        block,
        format!("{}\n\nOPENUI LANG GUIDE: Table, Chart, Steps.\n\n{}", *link_chat::APP_RULE, link_chat::APP_LINE)
    );
    assert!(link_chat::APP_RULE.starts_with("## Replying in the Eve Link app\n\n"));
    assert!(link_chat::APP_RULE.ends_with(link_chat::APP_EXAMPLE));
}

#[test]
fn the_shipped_guide_is_package_data() {
    assert!(Path::new(link_chat::PROMPT_PATH).exists());
    let block = link_chat::app_block(None);
    let text = std::fs::read_to_string(link_chat::PROMPT_PATH).unwrap();
    assert_eq!(block, format!("{}\n\n{}\n\n{}", *link_chat::APP_RULE, text.trim(), link_chat::APP_LINE));
}

// ---- link_chat: the chief's cards, for the Work view --------------------------------------------------------------

#[test]
fn the_job_lead_names_no_act() {
    // chief_card.verbs is task_router.CONSEQUENTIAL folded; that regex is read from its Rust port's source, since
    // this crate does not depend on eve-chief-card.
    let src = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../eve-desktop-lanes/src/task_router.rs"));
    let start = src.find("pub static CONSEQUENTIAL").unwrap();
    let body = &src[start..start + src[start..].find("});").unwrap()];
    let pattern: String = regex::Regex::new(r#"r"([^"]*)""#)
        .unwrap()
        .captures_iter(body)
        .map(|c| c[1].to_string())
        .collect();
    let consequential = regex::Regex::new(&pattern).unwrap();
    let verbs = |text: &str| -> BTreeSet<String> {
        consequential
            .find_iter(text)
            .map(|m| m.as_str().to_lowercase().split_whitespace().collect::<Vec<_>>().join(" "))
            .collect()
    };
    assert_eq!(verbs(&format!("{}{}", link_chat::JOB_LEAD, link_chat::JOB_DETAIL_LEAD)), BTreeSet::new());
    assert_eq!(verbs("order it"), BTreeSet::from(["order".to_string()])); // positive control
}

fn phase(n: i64, status: &str, door: &str, do_: &str) -> JobPhase {
    JobPhase { n, status: status.into(), door: door.into(), approval: None, do_: do_.into() }
}

/// A Standing desk card as test_chief's take() files it: research and assess done, the order (one way) queued, waiting
/// on its Go.
fn desk(cid: &str, updated: f64) -> JobCard {
    let Value::Object(data) = json!({"id": cid, "kind": "job", "status": "waiting", "goal": "Standing desk"}) else {
        unreachable!()
    };
    JobCard {
        id: cid.into(),
        kind: "job".into(),
        status: "waiting".into(),
        waiting_for: Some("go".into()),
        terminal: false,
        rev: 1,
        updated,
        phases: vec![phase(1, "ok", "two_way", "research"), phase(2, "ok", "two_way", "assess"), phase(3, "queued", "one_way", "act")],
        cue_at: None,
        doubtful: false,
        data,
    }
}

fn dropped(mut c: JobCard, updated: f64) -> JobCard {
    c.status = "dropped".into();
    c.waiting_for = None;
    c.terminal = true;
    c.updated = updated;
    c.data.insert("status".into(), json!("dropped"));
    c
}

struct FakeChief(Vec<JobCard>);

#[async_trait::async_trait]
impl Chief for FakeChief {
    fn cards(&self) -> Vec<JobCard> {
        self.0.clone()
    }
    fn card(&self, cid: &str) -> Option<JobCard> {
        self.0.iter().find(|c| c.id == cid).cloned()
    }
    async fn approve(&self, cid: &str, _via: &str, _rev: Option<i64>) -> anyhow::Result<Obj> {
        Err(anyhow::anyhow!("the fake chief only lists ({cid})"))
    }
    async fn done(&self, cid: &str) -> anyhow::Result<Obj> {
        Err(anyhow::anyhow!("the fake chief only lists ({cid})"))
    }
    async fn drop_job(&self, cid: &str) -> anyhow::Result<Obj> {
        Err(anyhow::anyhow!("the fake chief only lists ({cid})"))
    }
    async fn happened(&self, cid: &str) -> anyhow::Result<Obj> {
        Err(anyhow::anyhow!("the fake chief only lists ({cid})"))
    }
    async fn didnt(&self, cid: &str) -> anyhow::Result<Obj> {
        Err(anyhow::anyhow!("the fake chief only lists ({cid})"))
    }
    async fn raise_budget(&self, cid: &str) -> anyhow::Result<Obj> {
        Err(anyhow::anyhow!("the fake chief only lists ({cid})"))
    }
}

fn ids(got: &Obj) -> Vec<String> {
    got["jobs"].as_array().unwrap().iter().map(|j| j["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn jobs_list_open_cards_first_needs_you_first_then_the_finished() {
    let chief = FakeChief(vec![desk("c1", 100.0), desk("c2", 100.0), desk("c3", 100.0)]);
    let got = link_chat::list_jobs(&chief, &BTreeSet::new());
    assert_eq!((ids(&got), got["needs_you"].clone()), (vec!["c1".into(), "c2".into(), "c3".into()], json!(3)));
    let first = got["jobs"][0].as_object().unwrap();
    let keys: Vec<&str> = first.keys().map(String::as_str).collect();
    // Chief._as_data, plus
    assert_eq!(keys, ["id", "kind", "status", "goal", "rev", "updated", "line", "steps", "steps_done", "needs_you", "actions"]);
    assert_eq!((first["line"].clone(), first["needs_you"].clone()), (json!("Waiting on your Go"), json!(true)));
    assert_eq!(first["actions"], json!(["go", "drop"]));
    assert_eq!((first["steps"].clone(), first["steps_done"].clone(), first["rev"].clone()), (json!(3), json!(2), json!(1)));

    // c2 dropped a minute later; c3 approved a minute after that (not yet run: no longer needs you)
    let mut c3 = desk("c3", 220.0);
    c3.status = "active".into();
    c3.waiting_for = None;
    c3.phases[2].approval = Some(json!({"via": "id"}));
    let chief = FakeChief(vec![desk("c1", 100.0), dropped(desk("c2", 100.0), 160.0), c3]);
    let got = link_chat::list_jobs(&chief, &BTreeSet::new());
    assert_eq!((ids(&got), got["needs_you"].clone()), (vec!["c1".into(), "c3".into(), "c2".into()], json!(1)));
    let (c3, c2) = (&got["jobs"][1], &got["jobs"][2]);
    assert_eq!((c3["needs_you"].clone(), c3["actions"].clone(), c3["line"].clone()), (json!(false), json!(["drop"]), json!("Up next")));
    assert_eq!((c2["line"].clone(), c2["needs_you"].clone(), c2["actions"].clone()), (json!("Dropped"), json!(false), json!(["hide"])));
}

#[test]
fn a_finished_job_can_be_removed_from_work_for_good() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("chat").join("hidden-jobs.json");
    let hidden = HiddenJobs::new(Some(&path));
    assert!(hidden.ids().is_empty());
    hidden.add("c2").unwrap();
    let chief = FakeChief(vec![desk("c1", 1.0), dropped(desk("c2", 1.0), 2.0)]);
    assert_eq!(ids(&link_chat::list_jobs(&chief, &hidden.ids())), ["c1"]);
    let again = HiddenJobs::new(Some(&path)); // a restart reads it back
    assert_eq!(again.ids(), BTreeSet::from(["c2".to_string()]));
    assert!(!ids(&link_chat::list_jobs(&chief, &again.ids())).contains(&"c2".to_string()));
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    again.add("c10").unwrap();
    assert_eq!(std::fs::read_to_string(&path).unwrap(), r#"{"hidden": ["c10", "c2"]}"#); // sorted, as Python writes it
    std::fs::write(tmp.path().join("bad.json"), "{not json").unwrap();
    assert!(HiddenJobs::new(Some(&tmp.path().join("bad.json"))).ids().is_empty()); // unreadable: all shown again
    std::fs::write(tmp.path().join("list.json"), "[1]").unwrap();
    assert!(HiddenJobs::new(Some(&tmp.path().join("list.json"))).ids().is_empty());
    assert!(HiddenJobs::new(None).path.ends_with(".config/eve-bridge/chat/hidden-jobs.json"));
}

#[test]
fn only_the_last_twenty_finished_cards_are_listed() {
    let cards: Vec<JobCard> = (1..=23).map(|i| dropped(desk(&format!("c{i}"), 0.0), i as f64)).collect();
    let got = link_chat::list_jobs(&FakeChief(cards), &BTreeSet::new());
    let got_ids = ids(&got);
    assert_eq!((got_ids.len(), got["needs_you"].clone()), (link_chat::FINISHED_SHOWN, json!(0)));
    assert_eq!((got_ids[0].as_str(), got_ids[19].as_str()), ("c23", "c4")); // the newest finished, newest first
}

#[test]
fn an_act_that_may_have_happened_gets_no_go_only_the_question() {
    let mut c1 = desk("c1", 5.0);
    c1.doubtful = true;
    assert!(link_chat::job_needs_you(&c1));
    assert_eq!(link_chat::job_line(&c1), "May have happened: check");
    assert_eq!(link_chat::job_actions(&c1), ["happened", "didnt", "drop"]); // never "go"
    c1.doubtful = false; // it didn't: a new Go is asked
    assert_eq!(link_chat::job_actions(&c1), ["go", "drop"]);
    let chief = FakeChief(vec![desk("c3", 9.0), {
        let mut c = desk("c1", 5.0);
        c.doubtful = true;
        c
    }]);
    assert_eq!(ids(&link_chat::list_jobs(&chief, &BTreeSet::new())), ["c3", "c1"]); // both need the owner: newest first
}

#[test]
fn each_card_line_and_action_follows_its_state() {
    let mut c = desk("c1", 1.0);
    c.status = "unverified".into();
    c.waiting_for = None;
    assert_eq!((link_chat::job_line(&c), link_chat::job_needs_you(&c)), ("Not confirmed: did it go through?".into(), true));
    assert_eq!(link_chat::job_actions(&c), ["go", "done"]);
    c.status = "waiting".into();
    c.waiting_for = Some("budget".into());
    assert_eq!(link_chat::job_line(&c), "Waiting on a budget raise");
    assert_eq!(link_chat::job_actions(&c), ["go", "drop", "raise"]);
    c.waiting_for = Some("mac_slot".into());
    assert_eq!((link_chat::job_line(&c), link_chat::job_needs_you(&c)), ("Waiting for the Mac".into(), false));
    c.waiting_for = Some("other".into());
    assert_eq!(link_chat::job_line(&c), "Waiting");
    c.status = "proposed".into();
    assert_eq!(link_chat::job_line(&c), "Proposed");
    c.status = "active".into();
    c.phases[2].status = "running".into();
    assert_eq!(link_chat::job_line(&c), "Doing it");
    assert!(!link_chat::go_ready(&c)); // nothing queued
    c.phases[2].do_ = "dance".into();
    assert_eq!(link_chat::job_line(&c), "Working");
    c.phases = vec![];
    c.kind = "commitment".into();
    c.cue_at = Some("2026-10-14 09:00".into());
    assert_eq!(link_chat::job_line(&c), "Reminder 2026-10-14 09:00");
    assert_eq!(link_chat::job_actions(&c), ["done", "drop"]);
    c.cue_at = None;
    assert_eq!(link_chat::job_line(&c), "Working");
    // a Go is ready only when every step before the one-way step is done and it has no yes yet
    let mut d = desk("c2", 1.0);
    d.phases[1].status = "running".into();
    assert!(!link_chat::go_ready(&d));
    d.phases[1].status = "ok".into();
    assert!(link_chat::go_ready(&d));
    d.phases[2].approval = Some(json!(true));
    assert!(!link_chat::go_ready(&d));
}

// ---- link_chat: the controls listing ------------------------------------------------------------------------------

struct FakePanel {
    available: bool,
    listing: Vec<Value>,
}

#[async_trait::async_trait]
impl Hub for FakePanel {
    fn available(&self) -> bool {
        self.available
    }
    fn listing(&self) -> Vec<Value> {
        self.listing.clone()
    }
    async fn panel(&self, body: &Value) -> anyhow::Result<Value> {
        Ok(body.clone())
    }
}

fn hub(available: bool, listing: Vec<Value>) -> Arc<dyn Hub> {
    Arc::new(FakePanel { available, listing })
}

fn listing(hubs: &HashMap<&str, Arc<dyn Hub>>) -> Obj {
    link_chat::controls_listing(&|name: &str| hubs.get(name).cloned())
}

#[test]
fn controls_list_what_this_mac_has() {
    let mut hubs: HashMap<&str, Arc<dyn Hub>> = HashMap::new();
    let got = listing(&hubs);
    assert_eq!(
        Value::Object(got.clone()),
        json!({"lights": [], "spotify": false, "firetv": false, "memory": false, "notes": false, "pika": false,
               "obsbot": false, "apps": []})
    );
    let keys: Vec<&str> = got.keys().map(String::as_str).collect();
    assert_eq!(keys, ["lights", "spotify", "firetv", "memory", "notes", "pika", "obsbot", "apps"]);
    hubs.insert("lights", hub(true, vec![json!({"name": "desk lamp", "room": "office"}), json!({"name": "strip"})]));
    hubs.insert("spotify", hub(true, vec![]));
    assert_eq!(
        Value::Object(listing(&hubs)),
        json!({
            "lights": [{"name": "desk lamp", "room": "office"}, {"name": "strip"}], "spotify": true, "firetv": false,
            "memory": false, "notes": false, "pika": false, "obsbot": false,
            // the phone's Apps list (app_registry.for_phone): only what is here, in the phone's order
            "apps": [{"id": "spotify", "title": "Music", "symbol": "music.note", "detail": "Spotify", "sheet": "native"},
                     {"id": "lights", "title": "Lights", "symbol": "lightbulb", "detail": "2 lights", "sheet": "native"}]})
    );
    // the Memory card's object is always there (link_memory.py); it is listed only while its memory is built
    hubs.insert("memory", hub(false, vec![]));
    assert_eq!(listing(&hubs)["memory"], json!(false));
    hubs.insert("memory", hub(true, vec![]));
    assert_eq!(listing(&hubs)["memory"], json!(true));
    hubs.insert("notes", hub(true, vec![]));
    assert_eq!(listing(&hubs)["notes"], json!(true));
    // one light is "1 light"; lights with no listing are not on the phone's list
    hubs.insert("lights", hub(true, vec![json!({"name": "lamp"})]));
    assert_eq!(listing(&hubs)["apps"][1]["detail"], json!("1 light"));
    hubs.insert("lights", hub(true, vec![]));
    let apps: Vec<String> =
        listing(&hubs)["apps"].as_array().unwrap().iter().map(|a| a["id"].as_str().unwrap().to_string()).collect();
    assert_eq!(apps, ["spotify"]);
    // a card that is not available is not here
    hubs.insert("spotify", hub(false, vec![]));
    assert_eq!(listing(&hubs)["spotify"], json!(false));
}
