//! tests/test_meet.py: links, the caption merger, one call's state machine, the calendar and the notes. No
//! network, no browser: the page is a script of states (FakePage), the clock is a counter, sleep advances it.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use chrono::{DateTime, Duration, FixedOffset, NaiveDate, TimeZone};
use futures::future::BoxFuture;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use crate::*;

// ---- G3: links -----------------------------------------------------------------------------------

#[test]
fn test_a_meet_link_is_normalised() {
    for (text, url) in [
        ("https://meet.google.com/abc-defg-hij", "https://meet.google.com/abc-defg-hij?hl=en"),
        ("join meet.google.com/abc-defg-hij please", "https://meet.google.com/abc-defg-hij?hl=en"),
        ("<https://meet.google.com/ABC-DEFG-HIJ/>", "https://meet.google.com/abc-defg-hij?hl=en"),
        ("abc-defg-hij", "https://meet.google.com/abc-defg-hij?hl=en"),
        ("https://meet.google.com/abc-defg-hij?authuser=1&pli=1", "https://meet.google.com/abc-defg-hij?authuser=1&hl=en"),
        ("https://meet.google.com/lookup/team-sync", "https://meet.google.com/lookup/team-sync?hl=en"),
    ] {
        assert_eq!(parse_link(text), (Some(url.to_string()), String::new()), "{text}");
    }
}

#[test]
fn test_anything_else_is_refused_with_a_reason_link() {
    for text in [
        "http://meet.google.com/abc-defg-hij",
        "https://meet.google.com.evil.example/abc-defg-hij",
        "https://evilmeet.google.com/abc-defg-hij",
        "https://zoom.us/j/123456",
        "javascript:alert(1)",
        "https://user@meet.google.com/abc-defg-hij",
        "https://meet.google.com:8443/abc-defg-hij",
        "https://meet.google.com/",
        "https://meet.google.com/abc-defg-hij/extra",
        "standup",
        "abc-defg-hi",
        "",
    ] {
        let (url, why) = parse_link(text);
        assert!(url.is_none() && !why.is_empty(), "{text}");
    }
}

#[test]
fn test_the_meeting_code_labels_a_link_link() {
    assert_eq!(meeting_code("https://meet.google.com/abc-defg-hij?hl=en"), "abc-defg-hij");
}

// ---- G6: the caption merger ----------------------------------------------------------------------

pub(crate) fn snap(t: f64, rows: &[(i64, &str, &str)], self_id: i64) -> Value {
    json!({"t": t * 1000.0, "rows": rows.iter().map(|(i, s, x)| json!({"id": i, "speaker": s, "text": x, "self": *i == self_id}))
        .collect::<Vec<_>>()})
}

fn texts(lines: &[Line]) -> Vec<(String, String)> {
    lines.iter().map(|ln| (ln.speaker.clone(), ln.text.clone())).collect()
}

fn pairs(v: &[(&str, &str)]) -> Vec<(String, String)> {
    v.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect()
}

#[test]
fn test_merge_growth_scroll_revision_and_new_caption() {
    let m = |a: &str, b: &str| merge(a, b);
    assert_eq!(m("we should", "we should ship it").as_deref(), Some("we should ship it"));
    // the window scrolled: the front is gone, more at the end
    assert_eq!(
        m("one two three four five six", "four five six seven eight").as_deref(),
        Some("one two three four five six seven eight")
    );
    // the last words revised as Meet heard more
    assert_eq!(m("we should shift", "we should ship it today").as_deref(), Some("we should ship it today"));
    // shrank (a redraw): keep the fuller line
    assert_eq!(m("one two three four five", "three four five").as_deref(), Some("one two three four five"));
    // a different sentence: a new utterance
    assert_eq!(m("we should ship it", "what about the budget"), None);
    // live, 2026-09-25: Meet rewrote the first word, then inserted one mid-sentence
    let live = [
        "He will most likely ship on Friday.",
        "We will most likely ship on Friday. Um. Yeah, it's quite important that Sam gets this deck by the end of the week, um.",
        "We will most likely ship on Friday. Um. Yeah, it's. It's quite important that Sam gets this deck by the \
         end of the week, um. Hopefully, this is quite coherent to you. Thank you.",
    ];
    assert_eq!(m(live[0], live[1]).as_deref(), Some(live[1]));
    assert_eq!(m(live[1], live[2]).as_deref(), Some(live[2]));
}

#[test]
fn test_a_growing_caption_is_one_line_not_many() {
    let mut m = CaptionMerger::new();
    let mut out = Vec::new();
    for (k, words) in ["We", "We should", "We should ship", "We should ship on Friday."].iter().enumerate() {
        out.extend(m.feed(&snap(100.0 + k as f64 * 0.3, &[(1, "Alice", words)], -1)).unwrap());
    }
    out.extend(m.feed(&snap(103.0, &[], -1)).unwrap());
    assert_eq!(texts(&out), pairs(&[("Alice", "We should ship on Friday.")]));
}

#[test]
fn test_a_non_prefix_replacement_commits_the_old_line() {
    let mut m = CaptionMerger::new();
    let mut out = m.feed(&snap(1.0, &[(1, "Alice", "We should ship on Friday")], -1)).unwrap();
    out.extend(m.feed(&snap(2.0, &[(1, "Alice", "What about the budget")], -1)).unwrap());
    out.extend(m.finish());
    assert_eq!(texts(&out), pairs(&[("Alice", "We should ship on Friday"), ("Alice", "What about the budget")]));
}

#[test]
fn test_a_scrolling_monologue_is_stitched_into_paragraphs() {
    let words: Vec<String> = (0..150).map(|i| format!("w{i}")).collect();
    let mut m = CaptionMerger::new();
    let mut out = Vec::new();
    for end in (10usize..151).step_by(5) {
        // a window of the last 20 words, growing by 5
        let window = words[end.saturating_sub(20)..end].join(" ");
        out.extend(m.feed(&snap(end as f64 / 5.0, &[(7, "Bob", &window)], -1)).unwrap());
    }
    out.extend(m.finish());
    assert_eq!(out.iter().map(|ln| ln.text.clone()).collect::<Vec<_>>().join(" "), words.join(" "));
    assert!(out.iter().all(|ln| ln.speaker == "Bob") && out.len() >= 2); // paragraphs, not one wall
}

#[test]
fn test_own_tile_is_never_transcribed_caption() {
    let mut m = CaptionMerger::new();
    let mut out = m.feed(&snap(1.0, &[(1, "Alice", "hello"), (2, "You", "not me")], 2)).unwrap();
    out.extend(m.finish());
    assert_eq!(texts(&out), pairs(&[("Alice", "hello")]));
}

#[test]
fn test_a_re_rendered_block_continues_the_same_line_caption() {
    let mut m = CaptionMerger::new();
    let mut out = m.feed(&snap(1.0, &[(1, "Alice", "we should ship")], -1)).unwrap();
    out.extend(m.feed(&snap(1.3, &[(2, "Alice", "we should ship on Friday")], -1)).unwrap()); // the element was replaced
    out.extend(m.finish());
    assert_eq!(texts(&out), pairs(&[("Alice", "we should ship on Friday")]));
}

#[test]
fn test_two_speakers_interleave_caption() {
    let mut m = CaptionMerger::new();
    let mut out = m.feed(&snap(1.0, &[(1, "Alice", "shall we start")], -1)).unwrap();
    out.extend(m.feed(&snap(2.0, &[(1, "Alice", "shall we start"), (2, "Bob", "yes let's")], -1)).unwrap());
    out.extend(m.feed(&snap(4.0, &[(2, "Bob", "yes let's go")], -1)).unwrap());
    out.extend(m.finish());
    assert_eq!(texts(&out), pairs(&[("Alice", "shall we start"), ("Bob", "yes let's go")]));
}

#[test]
fn test_the_live_revisions_are_one_line_caption() {
    let mut m = CaptionMerger::new();
    let mut out = Vec::new();
    for (k, words) in [
        "He will most likely ship on Friday.",
        "We will most likely ship on Friday. Um. Yeah, it's quite important that Sam gets this deck",
        "We will most likely ship on Friday. Um. Yeah, it's. It's quite important that Sam gets this deck by the end \
         of the week. Thank you.",
    ]
    .iter()
    .enumerate()
    {
        out.extend(m.feed(&snap(100.0 + k as f64, &[(1, "Alex Chen", words)], -1)).unwrap());
    }
    out.extend(m.finish());
    assert_eq!(
        texts(&out),
        pairs(&[(
            "Alex Chen",
            "We will most likely ship on Friday. Um. Yeah, it's. It's quite important that Sam gets this deck by the \
             end of the week. Thank you."
        )])
    );
}

#[test]
fn test_a_pause_writes_the_line_down_and_more_words_add_only_the_rest_caption() {
    let mut m = CaptionMerger::new();
    let mut out = m.feed(&snap(1.0, &[(1, "Alice", "first thought")], -1)).unwrap();
    out.extend(m.tick(1.0 + IDLE_COMMIT_SECS));
    assert_eq!(texts(&out), pairs(&[("Alice", "first thought")]));
    out.extend(m.feed(&snap(10.0, &[(1, "Alice", "first thought and a second one")], -1)).unwrap());
    out.extend(m.finish());
    assert_eq!(texts(&out), pairs(&[("Alice", "first thought"), ("Alice", "and a second one")]));
}

// ---- G5: one call --------------------------------------------------------------------------------

/// The clock is a counter; sleep advances it and yields.
#[derive(Clone, Default)]
pub(crate) struct TestClock(pub Arc<Mutex<f64>>);

impl TestClock {
    pub fn now(&self) -> f64 {
        *self.0.lock()
    }
    pub fn clock(&self) -> Clock {
        let t = self.0.clone();
        Arc::new(move || *t.lock())
    }
    pub fn sleep(&self) -> Sleep {
        let t = self.0.clone();
        Arc::new(move |secs: f64| {
            *t.lock() += secs;
            Box::pin(tokio::task::yield_now()) as BoxFuture<'static, ()>
        })
    }
}

type StateFn = Arc<dyn Fn(&Value) -> Result<Value, String> + Send + Sync>;

/// A state is what the page script would return, an error, or a function of the options (leave).
#[derive(Clone)]
pub(crate) enum St {
    D(Value),
    F(StateFn),
    E(&'static str),
}

/// What the page saw, kept outside the page (which the session owns).
#[derive(Clone, Default)]
pub(crate) struct PageLog {
    pub opened: Arc<Mutex<Vec<String>>>,
    pub calls: Arc<Mutex<Vec<Value>>>,
    pub closed: Arc<AtomicBool>,
}

/// States in order; the last repeats.
pub(crate) struct FakePage {
    states: Vec<St>,
    i: Mutex<usize>,
    pub log: PageLog,
}

impl FakePage {
    pub fn new(states: Vec<St>) -> (FakePage, PageLog) {
        let log = PageLog::default();
        (FakePage { states, i: Mutex::new(0), log: log.clone() }, log)
    }
}

#[async_trait]
impl MeetPage for FakePage {
    async fn open(&self, url: &str) -> anyhow::Result<()> {
        self.log.opened.lock().push(url.to_string());
        Ok(())
    }

    async fn run(&self, opts: Value) -> anyhow::Result<Map<String, Value>> {
        self.log.calls.lock().push(opts.clone());
        let st = {
            let mut i = self.i.lock();
            let st = self.states[(*i).min(self.states.len() - 1)].clone();
            *i += 1;
            st
        };
        let v = match st {
            St::D(v) => v,
            St::F(f) => f(&opts).map_err(|e| eve_desktop_lanes::LaneError::err("RuntimeError", e))?,
            St::E(e) => return Err(eve_desktop_lanes::LaneError::err("RuntimeError", e)),
        };
        Ok(v.as_object().cloned().unwrap_or_default())
    }

    async fn close(&self) -> anyhow::Result<()> {
        self.log.closed.store(true, Ordering::SeqCst);
        Ok(())
    }
}

pub(crate) fn prejoin() -> St {
    St::D(json!({"inCall": false, "micOn": false, "camOn": false}))
}

pub(crate) fn in_call_v() -> Value {
    json!({"inCall": true, "captionsOn": true, "snaps": []})
}

pub(crate) fn in_call() -> St {
    St::D(in_call_v())
}

fn with(base: Value, extra: Value) -> St {
    let mut m = base.as_object().cloned().unwrap();
    m.extend(extra.as_object().cloned().unwrap());
    St::D(Value::Object(m))
}

fn ended(why: &str) -> St {
    St::D(json!({"inCall": false, "ended": why}))
}

/// The texts the owner got, and each one's history line.
#[derive(Clone, Default)]
pub(crate) struct Told {
    pub texts: Arc<Mutex<Vec<String>>>,
    pub abouts: Arc<Mutex<Vec<String>>>,
}

impl Told {
    pub fn notify(&self) -> Notify {
        let me = self.clone();
        Arc::new(move |text: String, about: String| {
            me.texts.lock().push(text);
            me.abouts.lock().push(about);
            Box::pin(async { Ok(()) }) as BoxFuture<'static, anyhow::Result<()>>
        })
    }
    pub fn texts(&self) -> Vec<String> {
        self.texts.lock().clone()
    }
    pub fn last(&self) -> String {
        self.texts.lock().last().cloned().unwrap_or_default()
    }
}

pub(crate) fn local(naive: chrono::NaiveDateTime) -> DateTime<FixedOffset> {
    eve_transcripts::py::local_aware(naive)
}

fn wall_1900() -> Wall {
    let t = local(NaiveDate::from_ymd_opt(2026, 9, 26).unwrap().and_hms_opt(19, 0, 0).unwrap());
    Arc::new(move || t)
}

struct Rig {
    s: MeetSession,
    log: PageLog,
    told: Told,
    clock: TestClock,
}

fn session(tmp: &Path, states: Vec<St>, config: MeetConfig, summarize: Option<Summarize>) -> Rig {
    let clock = TestClock::default();
    let told = Told::default();
    let (page, log) = FakePage::new(states);
    let c = clock.clone();
    let opts = SessionOptions {
        config,
        summarize,
        clock: clock.clock(),
        sleep: clock.sleep(),
        wall: wall_1900(),
        epoch: Arc::new(move || 1_000_000.0 + c.now()),
    };
    let s = MeetSession::new(
        "https://meet.google.com/abc-defg-hij?hl=en",
        "Standup",
        Box::new(page),
        &tmp.join("meetings"),
        told.notify(),
        opts,
    );
    Rig { s, log, told, clock }
}

fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn summary_of(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap()
}

fn caption_snap(seq: i64, t: f64, id: i64, speaker: &str, text: &str) -> Value {
    json!({"snaps": [{"seq": seq, "t": t * 1000.0, "rows": [{"id": id, "speaker": speaker, "text": text, "self": false}]}]})
}

#[tokio::test]
async fn test_session_joins_listens_and_writes_notes_when_the_call_ends() {
    let d = tmp();
    let summaries: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = summaries.clone();
    let summarize: Summarize = Arc::new(move |transcript: &str| {
        seen.lock().push(transcript.to_string());
        Ok(summary_of(json!({"title": "Ship date", "gist": "Picked Friday.", "points": [], "decisions": ["Ship Friday"],
                             "actions": ["Bob writes the notes"], "questions": [], "unclear": []})))
    });
    let t0 = 1_000_000.0;
    let states = vec![
        prejoin(),
        prejoin(),
        in_call(),
        with(in_call_v(), caption_snap(1, t0 + 5.0, 1, "Alice", "Ship Friday?")),
        with(in_call_v(), caption_snap(2, t0 + 7.0, 2, "Bob", "Yes, Friday.")),
        with(in_call_v(), json!({"snaps": []})),
        ended("call ended"),
    ];
    let r = session(d.path(), states, MeetConfig::default(), Some(summarize));
    let state = r.s.run().await;
    assert_eq!(*r.log.opened.lock(), vec!["https://meet.google.com/abc-defg-hij?hl=en".to_string()]);
    assert!(r.log.closed.load(Ordering::SeqCst));
    assert_eq!(state.ended, "Meet said: call ended");
    assert_eq!(state.lines, 2);
    let told = r.told.texts();
    assert!(told[0].starts_with("I'm in Standup") && told[0].contains("microphone and camera off"));
    let last = r.told.last();
    assert!(last.contains("Ship date") && last.contains("Ship Friday") && last.contains("Bob writes the notes"), "{last}");
    assert_eq!(*summaries.lock(), vec!["Alice: Ship Friday?\nBob: Yes, Friday.".to_string()]);
    let notes = state.notes.clone().unwrap();
    let body = std::fs::read_to_string(&notes).unwrap();
    assert_eq!(notes.parent().unwrap(), d.path().join("meetings").join("2026-09-26"));
    assert!(body.contains("# Ship date") && body.contains("**Alice**") && body.contains("**Bob**") && body.contains("Ship Friday"));
    assert_eq!(std::fs::metadata(&notes).unwrap().permissions().mode() & 0o777, 0o600);
    // every poll before the call asks for no captions; every poll in it asks for them, with the cursor
    let calls = r.log.calls.lock().clone();
    assert!(calls.iter().all(|c| c.get("act").is_some_and(|v| v == &json!(true))));
    let since: Vec<Value> = calls.iter().filter(|c| c.get("captions") == Some(&json!(true))).map(|c| c["since"].clone()).collect();
    assert_eq!(since, vec![json!(0), json!(1), json!(2), json!(2)]);
}

#[tokio::test]
async fn test_session_lobby_tells_the_owner_once_and_waits() {
    let d = tmp();
    let lobby = St::D(json!({"inCall": false, "lobby": true}));
    let mut states = vec![prejoin()];
    states.extend(std::iter::repeat_n(lobby, 100));
    states.extend([in_call(), in_call(), ended("call ended")]);
    let r = session(d.path(), states, MeetConfig::default(), None);
    r.s.run().await;
    let told = r.told.texts();
    assert_eq!(told.iter().filter(|t| t.contains("waiting to be let into Standup")).count(), 1);
    assert!(told.iter().any(|t| t.starts_with("I'm in Standup"))); // admitted after 100 s in the lobby
}

#[tokio::test]
async fn test_session_lobby_gives_up_after_the_limit() {
    let d = tmp();
    let left = Arc::new(AtomicBool::new(false));
    let l = left.clone();
    let lobby = St::F(Arc::new(move |opts: &Value| {
        if opts.get("leave").is_some_and(|v| v == &json!(true)) {
            l.store(true, Ordering::SeqCst);
            return Ok(json!({"inCall": false}));
        }
        Ok(json!({"inCall": false, "lobby": true}))
    }));
    let cfg = MeetConfig { lobby_minutes: 2.0, ..Default::default() };
    let r = session(d.path(), vec![lobby], cfg, None);
    let state = r.s.run().await;
    assert!(state.ended.contains("nobody let me in within 2 minutes") && left.load(Ordering::SeqCst), "{}", state.ended);
    assert!(r.told.last().starts_with("I couldn't join Standup: nobody let me in") && r.log.closed.load(Ordering::SeqCst));
    // v3 safety review: the invite's title is outside words; the history keeps code's own line
    let about = r.told.abouts.lock().last().cloned().unwrap();
    assert!(!about.is_empty() && !about.contains("Standup"));
    let t = r.clock.now();
    assert!((120.0..125.0).contains(&t), "{t}");
}

#[tokio::test]
async fn test_session_ends_with_a_reason_the_owner_is_told() {
    for (st, why) in [
        (json!({"inCall": false, "ended": "no one responded to your request"}), "Meet said: no one responded"),
        (json!({"inCall": false, "ended": "you can't join this call"}), "Meet said: you can't join"),
        (json!({"inCall": false, "signIn": true}), "signed out"),
        (json!({"closed": true}), "the tab was closed"),
    ] {
        let d = tmp();
        let r = session(d.path(), vec![prejoin(), St::D(st)], MeetConfig::default(), None);
        let state = r.s.run().await;
        assert!(state.ended.contains(why) && r.told.last().contains(why) && r.log.closed.load(Ordering::SeqCst), "{why}");
        assert!(state.notes.is_none()); // never in: no notes file
    }
}

#[tokio::test]
async fn test_session_join_timeout_names_why() {
    let d = tmp();
    let r = session(d.path(), vec![St::D(json!({"inCall": false, "switchHere": true, "joinHereToo": false}))],
                    MeetConfig::default(), None);
    let state = r.s.run().await;
    assert!(state.ended.contains("won't take it from you") && r.clock.now() >= JOIN_SECS);
    let r = session(d.path(), vec![St::D(json!({"inCall": false, "micOn": true}))], MeetConfig::default(), None);
    assert!(r.s.run().await.ended.contains("couldn't turn the microphone and camera off"));
}

#[tokio::test]
async fn test_session_no_controls_is_allowed_only_after_a_while() {
    let d = tmp();
    let mut states: Vec<St> = std::iter::repeat_n(prejoin(), 30).collect();
    states.extend([in_call(), ended("call ended")]);
    let r = session(d.path(), states, MeetConfig::default(), None);
    r.s.run().await;
    let flags: Vec<Value> = r.log.calls.lock().iter().filter_map(|c| c.get("noControlsOk").cloned()).collect();
    assert_eq!(flags[0], json!(false));
    assert_eq!(flags[NO_CONTROLS_SECS as usize + 1], json!(true));
}

#[tokio::test]
async fn test_session_leave_presses_leave_and_writes_notes() {
    let d = tmp();
    let leaves = Arc::new(Mutex::new(0usize));
    let l = leaves.clone();
    let state_fn = St::F(Arc::new(move |opts: &Value| {
        if opts.get("leave").is_some_and(|v| v == &json!(true)) {
            *l.lock() += 1;
            return Ok(json!({"inCall": *l.lock() < 2}));
        }
        Ok(in_call_v())
    }));
    let r = session(d.path(), vec![prejoin(), state_fn], MeetConfig::default(), None);
    let handle = r.s.handle();
    let task = tokio::spawn(r.s.run());
    while handle.state().phase != "in call" {
        tokio::task::yield_now().await;
    }
    handle.leave();
    let state = task.await.unwrap();
    assert_eq!(state.ended, "you asked me to leave");
    assert_eq!(*leaves.lock(), 2);
    assert!(r.log.closed.load(Ordering::SeqCst));
    assert!(r.told.last().contains("Nothing was said")); // an empty call: no model call
    let notes = state.notes.unwrap();
    assert!(notes.exists() && std::fs::read_to_string(notes).unwrap().contains("nothing was said"));
}

#[tokio::test]
async fn test_session_redraw_is_not_the_end() {
    let d = tmp();
    let out = St::D(json!({"inCall": false}));
    let r = session(d.path(), vec![prejoin(), in_call(), out.clone(), in_call(), out.clone(), out.clone(), out],
                    MeetConfig::default(), None);
    let state = r.s.run().await;
    assert_eq!(state.ended, "the call ended");
    assert!(state.joined_at.is_some());
}

#[tokio::test]
async fn test_session_captions_missing_is_told_once() {
    let d = tmp();
    let no_caps = St::D(json!({"inCall": true, "captionsOn": false, "snaps": []}));
    let mut states = vec![prejoin()];
    states.extend(std::iter::repeat_n(no_caps, 80));
    states.push(ended("call ended"));
    let r = session(d.path(), states, MeetConfig::default(), None);
    r.s.run().await;
    assert_eq!(r.told.texts().iter().filter(|t| t.contains("Captions won't turn on")).count(), 1);
}

/// Round-4 review: "I'm in {label}", the lobby and the captions texts went into the history as eve's own words,
/// and the label can be a calendar invite's title. Each now carries an `about` without the label.
#[tokio::test]
async fn test_every_text_naming_the_meeting_leaves_the_history_code_s_own_line() {
    let d = tmp();
    let lobby = St::D(json!({"inCall": false, "lobby": true}));
    let no_caps = St::D(json!({"inCall": true, "captionsOn": false, "snaps": []}));
    let mut states = vec![prejoin(), lobby.clone(), lobby];
    states.extend(std::iter::repeat_n(no_caps, 80));
    states.push(ended("x"));
    let r = session(d.path(), states, MeetConfig::default(), None);
    r.s.run().await;
    let (told, abouts) = (r.told.texts(), r.told.abouts.lock().clone());
    assert_eq!(told.len(), abouts.len());
    let named: Vec<(&String, &String)> = told.iter().zip(abouts.iter()).filter(|(t, _)| t.contains("Standup")).collect();
    assert!(named.len() >= 4); // the lobby, in, captions, the notes
    for (text, about) in named {
        assert!(!about.is_empty() && !about.contains("Standup"), "{text}");
    }
}

#[tokio::test]
async fn test_session_a_page_that_stops_answering_ends_it() {
    let d = tmp();
    let mut states = vec![prejoin(), in_call()];
    states.extend(std::iter::repeat_n(St::E("target closed"), 10));
    let r = session(d.path(), states, MeetConfig::default(), None);
    let state = r.s.run().await;
    assert!(state.ended.contains("stopped answering") && r.log.closed.load(Ordering::SeqCst), "{}", state.ended);
    assert_eq!(state.ended, "the Meet tab stopped answering (RuntimeError)");
}

#[tokio::test]
async fn test_session_the_transcript_is_on_disk_before_the_end() {
    let d = tmp();
    let t0 = 1_000_000.0;
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let summarize: Summarize = Arc::new(|_t: &str| Err(anyhow::anyhow!("model down")));
    let handle: Arc<OnceLock<SessionHandle>> = Arc::default();
    let (h, s2) = (handle.clone(), seen.clone());
    let later = St::F(Arc::new(move |_opts: &Value| {
        let notes = h.get().and_then(|h| h.state().notes);
        s2.lock().push(notes.map(|p| std::fs::read_to_string(p).unwrap_or_default()).unwrap_or_default());
        Ok(json!({"inCall": false, "ended": "call ended"}))
    }));
    let states = vec![
        prejoin(),
        in_call(),
        with(in_call_v(), caption_snap(1, t0 + 1.0, 1, "Alice", "Budget first.")),
        with(in_call_v(), json!({"snaps": []})),
        with(in_call_v(), json!({"snaps": []})),
        with(in_call_v(), json!({"snaps": []})),
        later,
    ];
    let r = session(d.path(), states, MeetConfig::default(), Some(summarize));
    let _ = handle.set(r.s.handle());
    let state = r.s.run().await;
    assert!(seen.lock()[0].contains("Budget first.")); // written while the call was on
    assert!(r.told.last().contains("write-up failed"));
    assert!(std::fs::read_to_string(state.notes.unwrap()).unwrap().contains("Budget first."));
}

#[test]
fn test_notes_two_calls_in_one_minute_get_two_files() {
    let d = tmp();
    let started = local(NaiveDate::from_ymd_opt(2026, 9, 26).unwrap().and_hms_opt(19, 0, 0).unwrap());
    let a = NotesFile::new(&d.path().join("meetings"), started, "abc-defg-hij").unwrap();
    let b = NotesFile::new(&d.path().join("meetings"), started, "abc-defg-hij").unwrap();
    assert_ne!(a.path, b.path);
    assert_eq!(a.path.file_name().unwrap(), "1900-meet-abc-defg-hij.md");
    assert!(b.path.to_string_lossy().ends_with("-2.md"));
}

// ---- G7: the calendar ----------------------------------------------------------------------------

pub(crate) fn pt() -> FixedOffset {
    FixedOffset::west_opt(7 * 3600).unwrap()
}

fn isoformat(t: DateTime<FixedOffset>) -> String {
    t.to_rfc3339()
}

pub(crate) fn ev_at(off: FixedOffset, title: &str, h: u32, m: u32, mins: i64, link: &str, extra: Value) -> Value {
    let start = off.with_ymd_and_hms(2026, 9, 26, h, m, 0).unwrap();
    let mut e = json!({"summary": title, "start": {"dateTime": isoformat(start)},
                       "end": {"dateTime": isoformat(start + Duration::minutes(mins))}});
    let map = e.as_object_mut().unwrap();
    if let Some(x) = extra.as_object() {
        map.extend(x.clone());
    }
    if !link.is_empty() {
        map.insert("hangoutLink".into(), json!(link));
    }
    e
}

fn ev(title: &str, h: u32, link: &str) -> Value {
    ev_at(pt(), title, h, 0, 30, link, json!({}))
}

fn events_at(off: FixedOffset) -> Vec<Value> {
    vec![
        ev_at(off, "Standup", 9, 0, 30, "https://meet.google.com/sta-ndup-aaa", json!({})),
        ev_at(off, "Lunch", 12, 0, 30, "", json!({})),
        ev_at(off, "Design review", 15, 0, 30, "https://meet.google.com/des-ignr-evw", json!({})),
        ev_at(off, "Test meeting", 19, 0, 30, "https://meet.google.com/tes-tmee-tng", json!({})),
        json!({"summary": "Holiday", "start": {"date": "2026-09-26"}, "end": {"date": "2026-09-27"}}),
    ]
}

fn at_pt(h: u32, m: u32) -> DateTime<FixedOffset> {
    pt().with_ymd_and_hms(2026, 9, 26, h, m, 0).unwrap()
}

#[test]
fn test_calendar_finds_the_meeting() {
    let events = events_at(pt());
    for (words, now, title) in [
        ("now", (9, 10), "Standup"),
        ("", (18, 55), "Test meeting"), // starts within the hour
        ("next", (12, 30), "Design review"),
        ("3pm", (8, 0), "Design review"),
        ("my 3", (8, 0), "Design review"),
        ("7 pm", (8, 0), "Test meeting"),
        ("19:00", (8, 0), "Test meeting"),
        ("the design review", (8, 0), "Design review"),
    ] {
        let (e, link, why) = find_meeting(&events, words, at_pt(now.0, now.1));
        let e = e.unwrap_or_else(|| panic!("{words}: {why}"));
        assert!(e["summary"] == title && link.starts_with("https://meet.google.com/") && why.is_empty(), "{words}");
    }
}

#[test]
fn test_calendar_reads_dotted_pm() {
    // "p.m." used to fall through the trailing \b, read as a bare 9, and join the 9 AM meeting
    let events = vec![ev("Standup", 9, "https://meet.google.com/sta-ndup-aaa"), ev("Late sync", 21, "https://meet.google.com/lat-esyn-ccc")];
    for words in ["join at 9 p.m.", "join at 9 P.M.", "9pm"] {
        let (e, _link, why) = find_meeting(&events, words, at_pt(8, 0));
        assert!(e.is_some_and(|e| e["summary"] == "Late sync") && why.is_empty(), "{words}");
    }
}

#[test]
fn test_calendar_reads_dotted_am() {
    let events = vec![ev("Standup", 9, "https://meet.google.com/sta-ndup-aaa"), ev("Late sync", 21, "https://meet.google.com/lat-esyn-ccc")];
    let (e, _link, _why) = find_meeting(&events, "join at 9 a.m.", at_pt(8, 0));
    assert!(e.is_some_and(|e| e["summary"] == "Standup"));
}

#[test]
fn test_calendar_no_match_is_a_plain_reason() {
    let events = events_at(pt());
    for (words, now) in [("now", (12, 10)), ("noon", (8, 0)), ("lunch", (8, 0)), ("11am", (8, 0))] {
        let (e, link, why) = find_meeting(&events, words, at_pt(now.0, now.1));
        assert!(e.is_none() && link.is_empty() && !why.is_empty(), "{words}");
    }
}

#[test]
fn test_calendar_reads_the_link_from_conference_data_and_nested_replies() {
    let e = ev_at(pt(), "Sync", 10, 0, 30, "", json!({"conferenceData": {"entryPoints": [
        {"entryPointType": "phone", "uri": "tel:+1-555"},
        {"entryPointType": "video", "uri": "https://meet.google.com/syn-csyn-cxx"}]}}));
    let reply = json!({"successful": true, "data": {"response_data": serde_json::to_string(&json!({"items": [e]})).unwrap()}});
    let found = events_in(&reply, 0);
    assert_eq!(found, vec![e.clone()]);
    assert_eq!(event_link(&e).as_deref(), Some("https://meet.google.com/syn-csyn-cxx?hl=en"));
}

pub(crate) fn local_offset() -> FixedOffset {
    *local(NaiveDate::from_ymd_opt(2026, 9, 26).unwrap().and_hms_opt(12, 0, 0).unwrap()).offset()
}

type Asked = Arc<Mutex<Vec<(DateTime<FixedOffset>, DateTime<FixedOffset>)>>>;

#[tokio::test]
async fn test_calendar_via_the_meeter_resolves_and_joins() {
    let d = tmp();
    let clock = TestClock::default();
    let pages: Arc<Mutex<Vec<PageLog>>> = Arc::default();
    let p = pages.clone();
    let make_page: MakePage = Arc::new(move || {
        let (page, log) = FakePage::new(vec![prejoin(), ended("call ended")]);
        p.lock().push(log);
        Ok(Box::new(page) as Box<dyn MeetPage>)
    });
    let asked: Asked = Arc::default();
    let a = asked.clone();
    // the Python test's times are Pacific and the Mac is in Pacific; the Meeter reads "3pm" in the Mac's zone, so
    // the events here are in the Mac's own zone, whichever it is
    let off = local_offset();
    let events = events_at(off);
    let list_events: ListEvents = Arc::new(move |start, end| {
        a.lock().push((start, end));
        Ok(json!({"data": {"items": events.clone()}}))
    });
    let wall_t = off.with_ymd_and_hms(2026, 9, 26, 14, 50, 0).unwrap();
    let opts = MeeterOptions {
        list_events: Some(list_events),
        clock: clock.clock(),
        sleep: clock.sleep(),
        wall: Arc::new(move || wall_t),
        ..Default::default()
    };
    let m = Meeter::new(MeetConfig::default(), make_page, d.path(), opts);
    let r = m.join("3pm").await.unwrap();
    assert!(r["ok"] == json!(true) && r["joining"] == "Design review" && r["link"].as_str().unwrap().contains("des-ignr-evw"));
    let again = m.join("https://meet.google.com/abc-defg-hij").await.unwrap();
    assert!(again["ok"] == json!(false) && again["reason"].as_str().unwrap().contains("already in Design review"));
    m.wait().await;
    let asked = asked.lock().clone();
    assert!(!asked.is_empty() && asked[0].1 - asked[0].0 == Duration::days(1));
    assert_eq!(*pages.lock()[0].opened.lock(), vec!["https://meet.google.com/des-ignr-evw?hl=en".to_string()]);
    assert!(!m.busy() && m.status_line().contains("not in a meeting"));
}

fn no_page() -> MakePage {
    Arc::new(|| Err(anyhow::anyhow!("no page")))
}

#[tokio::test]
async fn test_calendar_meeter_without_a_calendar_asks_for_the_link() {
    let d = tmp();
    let m = Meeter::new(MeetConfig::default(), no_page(), d.path(), MeeterOptions::default());
    let r = m.join("my 3pm").await.unwrap();
    assert!(r["ok"] == json!(false) && r["reason"].as_str().unwrap().contains("link"));
    let r = m.join("http://meet.google.com/abc-defg-hij").await.unwrap();
    assert!(r["ok"] == json!(false) && r["reason"].as_str().unwrap().contains("https"));
}

#[tokio::test]
async fn test_meeter_tools_and_off_switch() {
    let d = tmp();
    let m = Meeter::new(MeetConfig { enabled: false, ..Default::default() }, no_page(), d.path(), MeeterOptions::default());
    let names: Vec<Value> = m.tools().iter().map(|t| t["name"].clone()).collect();
    assert_eq!(names, vec![json!("meet_join"), json!("meet_status"), json!("meet_leave")]);
    assert_eq!(m.join("abc-defg-hij").await.unwrap()["ok"], json!(false));
    assert_eq!(m.handle("meet_leave", &json!({})).await.unwrap(), json!({"ok": false, "reason": "I'm not in a meeting"}));
    let env = |pairs: &[(&str, &str)]| pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    assert!(!configured(Some(&env(&[("EVE_MEET", "0")]))).enabled);
    let cfg = configured(Some(&env(&[("EVE_MEET_PROFILE", "Eve@Example.com"), ("EVE_MEET_LOBBY_MINUTES", "999")])));
    assert!(cfg.profile == "eve@example.com" && cfg.lobby_minutes == 120.0 && cfg.quiet);
}

// ---- beyond the Python file: the owner's side end to end -------------------------------------------

#[tokio::test]
async fn meeter_status_leave_and_close_follow_the_call() {
    let d = tmp();
    let clock = TestClock::default();
    let told = Told::default();
    let left = Arc::new(AtomicBool::new(false));
    let l = left.clone();
    let make_page: MakePage = Arc::new(move || {
        let l = l.clone();
        let state = St::F(Arc::new(move |opts: &Value| {
            if opts.get("leave").is_some_and(|v| v == &json!(true)) {
                l.store(true, Ordering::SeqCst);
            }
            Ok(json!({"inCall": !l.load(Ordering::SeqCst), "captionsOn": true, "snaps": []}))
        }));
        Ok(Box::new(FakePage::new(vec![state]).0) as Box<dyn MeetPage>)
    });
    let opts = MeeterOptions {
        notify: Some(told.notify()),
        clock: clock.clock(),
        sleep: clock.sleep(),
        wall: wall_1900(),
        ..Default::default()
    };
    let m = Meeter::new(MeetConfig::default(), make_page, d.path(), opts);
    assert_eq!(m.status(), json!({"ok": true, "in_meeting": false}));
    assert!(m.status_line().starts_with("I'm not in a meeting."));
    let r = m.handle("meet_join", &json!({"meeting": " abc-defg-hij "})).await.unwrap();
    assert_eq!(r["joining"], "abc-defg-hij");
    assert_eq!(m.status_line(), "I'm joining abc-defg-hij. /meet leave takes me out.");
    while m.session().unwrap().state().phase != "in call" {
        tokio::task::yield_now().await;
    }
    let st = m.handle("meet_status", &json!({})).await.unwrap();
    assert_eq!(st["in_meeting"], json!(true));
    assert_eq!(st["phase"], "in call");
    assert_eq!(st["notes_file"], "1900-meet-abc-defg-hij.md");
    assert!(m.status_line().starts_with("I'm in abc-defg-hij, "), "{}", m.status_line());
    assert_eq!(m.handle("meet_leave", &json!({})).await.unwrap(), json!({"ok": true, "leaving": "abc-defg-hij"}));
    let state = m.wait().await.unwrap();
    assert_eq!(state.ended, "you asked me to leave");
    assert!(left.load(Ordering::SeqCst));
    assert_eq!(m.handle("nope", &json!({})).await.unwrap(), json!({"ok": false, "reason": "unknown tool nope"}));

    // close: the call is stopped, and the notes are still written and texted
    m.join("xyz-abcd-efg").await.unwrap();
    left.store(false, Ordering::SeqCst);
    while m.session().unwrap().state().phase != "in call" {
        tokio::task::yield_now().await;
    }
    m.close().await;
    let s = m.session().unwrap().state();
    assert_eq!((s.phase.as_str(), s.ended.as_str()), ("done", "eve was shutting down"));
    assert!(told.last().contains("eve was shutting down"), "{}", told.last());
}

#[tokio::test]
async fn a_session_cancelled_mid_call_says_eve_stopped_and_still_writes_the_notes() {
    let d = tmp();
    let r = session(d.path(), vec![prejoin(), in_call()], MeetConfig::default(), None);
    let handle = r.s.handle();
    let task = tokio::spawn(r.s.run());
    while handle.state().phase != "in call" {
        tokio::task::yield_now().await;
    }
    handle.cancel();
    let state = task.await.unwrap();
    assert_eq!(state.ended, "eve stopped");
    assert!(state.notes.unwrap().exists() && r.log.closed.load(Ordering::SeqCst));
    assert!(r.told.last().starts_with("Standup is over (eve stopped)."));
}

#[tokio::test]
async fn a_page_that_fails_to_open_is_told_as_it_failed() {
    struct Refuses;
    #[async_trait]
    impl MeetPage for Refuses {
        async fn open(&self, _url: &str) -> anyhow::Result<()> {
            Err(eve_desktop_lanes::LaneError::err("AttachError", "Chrome said no"))
        }
        async fn run(&self, _opts: Value) -> anyhow::Result<Map<String, Value>> {
            Ok(Map::new())
        }
        async fn close(&self) -> anyhow::Result<()> {
            Err(anyhow::anyhow!("already closed"))
        }
    }
    let d = tmp();
    let told = Told::default();
    let s = MeetSession::new("https://meet.google.com/abc-defg-hij?hl=en", "Standup", Box::new(Refuses),
                             d.path(), told.notify(), SessionOptions::default());
    let state = s.run().await;
    assert_eq!(state.ended, "it failed: Chrome said no");
    assert_eq!(told.last(), "I couldn't join Standup: it failed: Chrome said no.");
}
