//! diary: the memory stream, retrieval, the pick, the write gate, the
//! appraisal → emote, and reflection — all against a fake client.
//! Port of bridge/tests/test_diary.py, plus the diary parts of
//! test_model_memory_roles.py, test_spend_wiring.py and test_daemon_membus.py.

use super::*;
use chrono::NaiveDate;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

use eve_explore::{THUMB_H, THUMB_W};

const W: i64 = 160;
const H: i64 = 120;

fn frame(level: u8) -> Frame {
    Frame::new(1, W, H, "gray", vec![level; (W * H) as usize])
}

fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> NaiveDateTime {
    NaiveDate::from_ymd_opt(y, mo, d).unwrap().and_hms_opt(h, mi, s).unwrap()
}

fn strs(xs: &[&str]) -> Vec<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

fn set(xs: &[&str]) -> BTreeSet<String> {
    xs.iter().map(|s| s.to_string()).collect()
}

/// `asyncio.run`: a fresh event loop for each call; pending tasks die with it.
fn run<F: std::future::Future>(f: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(f)
}

/// The reply the model would give: `_reply(...)` with its defaults.
#[derive(Clone)]
struct Rp {
    thoughts: Vec<(String, f64)>,
    novelty: i64,
    importance: i64,
    valence: i64,
    arousal: i64,
    label: &'static str,
    tags: Vec<&'static str>,
    observations: Vec<&'static str>,
    changed: Vec<&'static str>,
    photo: Option<Value>,
}

fn rp(thoughts: &[(&str, f64)]) -> Rp {
    Rp {
        thoughts: thoughts.iter().map(|(t, p)| (t.to_string(), *p)).collect(),
        novelty: 6,
        importance: 3,
        valence: 20,
        arousal: 30,
        label: "curious",
        tags: vec!["mug", "lamp"],
        observations: vec!["blue mug on the left"],
        changed: vec!["mug moved"],
        photo: None,
    }
}

impl Rp {
    fn s(&self) -> String {
        let mut d = json!({
            "observations": self.observations, "changed": self.changed,
            "thoughts": self.thoughts.iter().map(|(t, p)| json!({"text": t, "p": p})).collect::<Vec<_>>(),
            "novelty": self.novelty, "importance": self.importance, "tags": self.tags,
            "valence": self.valence, "arousal": self.arousal, "label": self.label,
        });
        if let Some(p) = &self.photo {
            d["photo"] = p.clone();
        }
        d.to_string()
    }
}

#[derive(Default)]
struct FakeClient {
    replies: StdMutex<VecDeque<String>>,
    reflection: Option<String>,
    examination: Option<String>,
    delay: Option<Duration>,
    contexts: StdMutex<Vec<String>>,
    reflect_prompts: StdMutex<Vec<String>>,
    examine_contexts: StdMutex<Vec<String>>,
}

fn fake(replies: Vec<String>) -> FakeClient {
    FakeClient { replies: StdMutex::new(replies.into()), ..Default::default() }
}

impl FakeClient {
    fn push(&self, reply: String) {
        self.replies.lock().unwrap().push_back(reply);
    }
    fn contexts(&self) -> Vec<String> {
        self.contexts.lock().unwrap().clone()
    }
    fn reflect_prompts(&self) -> Vec<String> {
        self.reflect_prompts.lock().unwrap().clone()
    }
    fn examine_contexts(&self) -> Vec<String> {
        self.examine_contexts.lock().unwrap().clone()
    }
}

impl DiaryClient for FakeClient {
    fn think(&self, _image: &[u8], _mime: &str, context: &str) -> anyhow::Result<String> {
        self.contexts.lock().unwrap().push(context.to_string());
        if let Some(d) = self.delay {
            std::thread::sleep(d);
        }
        self.replies.lock().unwrap().pop_front().ok_or_else(|| RuntimeError("out of replies".into()).into())
    }

    fn reflect(&self, prompt: &str) -> anyhow::Result<String> {
        self.reflect_prompts.lock().unwrap().push(prompt.to_string());
        self.reflection.clone().ok_or_else(|| RuntimeError("no reflection".into()).into())
    }

    /// The second, unhurried look at a photo eve kept.
    fn examine(&self, _image: &[u8], _mime: &str, context: &str) -> anyhow::Result<String> {
        self.examine_contexts.lock().unwrap().push(context.to_string());
        self.examination.clone().ok_or_else(|| RuntimeError("no examination".into()).into())
    }
}

#[derive(Clone)]
struct TestClock(Arc<StdMutex<NaiveDateTime>>);

impl TestClock {
    fn new(start: NaiveDateTime) -> Self {
        TestClock(Arc::new(StdMutex::new(start)))
    }
    fn wall(&self) -> Wall {
        let c = self.0.clone();
        Arc::new(move || *c.lock().unwrap())
    }
    fn now(&self) -> NaiveDateTime {
        *self.0.lock().unwrap()
    }
    fn tick(&self, d: chrono::Duration) {
        let mut g = self.0.lock().unwrap();
        *g += d;
    }
}

type Emotes = Arc<StdMutex<Vec<Emote>>>;

fn taker(tmp: &Path, client: &Arc<FakeClient>, clock: &TestClock, emotes: Option<Emotes>) -> Arc<DiaryTaker> {
    let send: SendEmote = Arc::new(move |e: Emote| {
        let em = emotes.clone();
        Box::pin(async move {
            if let Some(v) = em {
                v.lock().unwrap().push(e);
            }
            Ok(())
        })
    });
    Arc::new(
        DiaryTaker::new(client.clone(), tmp.join("notes"))
            .with_send_emote(send)
            .with_wall(clock.wall())
            .with_clock(Arc::new(|| 0.0)),
    )
}

fn note(frame: Frame, yaw: i64, pitch: i64) -> Note {
    Note::new(frame, yaw, pitch)
}

fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, String) {
    #[derive(Clone)]
    struct Buf(Arc<StdMutex<Vec<u8>>>);
    impl std::io::Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf(Arc::new(StdMutex::new(Vec::new())));
    let writer = buf.clone();
    let sub = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::DEBUG)
        .finish();
    let r = tracing::subscriber::with_default(sub, f);
    let out = buf.0.lock().unwrap().clone();
    (r, String::from_utf8(out).unwrap())
}

fn read(p: &Path) -> String {
    std::fs::read_to_string(p).unwrap()
}

// ---- pure pieces --------------------------------------------------------------------------------

#[test]
fn test_words_and_jaccard() {
    assert_eq!(words("The blue mug's gone, isn't it?"), set(&["the", "blue", "mug's", "gone", "isn't", "it"]));
    assert_eq!(jaccard(&set(&["a", "b"]), &set(&["b", "c"])), 1.0 / 3.0);
    assert_eq!(jaccard(&BTreeSet::new(), &set(&["a"])), 0.0);
}

#[test]
fn test_parse_reply_tolerates_fences_and_prose() {
    assert_eq!(parse_reply("```json\n{\"a\": 1}\n```").unwrap()["a"], json!(1));
    assert_eq!(parse_reply("Sure! {\"thoughts\": []} there").unwrap()["thoughts"], json!([]));
}

#[test]
fn test_pick_thought_prefers_least_typical_specific_non_repeat() {
    let recent = strs(&["The blue mug is on the left again."]);
    let baseline = words("a desk with a monitor and a keyboard and the blue mug");
    let cands = vec![
        json!({"text": "A desk with a monitor and a keyboard.", "p": 0.9}), // generic: only baseline words
        json!({"text": "The blue mug is on the left again today.", "p": 0.4}), // near-repeat of recent
        json!({"text": "Someone finished the coffee and left the spoon.", "p": 0.2}),
    ];
    assert_eq!(
        pick_thought(&cands, &recent, &baseline).unwrap().as_deref(),
        Some("Someone finished the coffee and left the spoon.")
    );
    // only generic candidates: the least typical still wins as a fallback
    let generic = vec![json!({"text": "A desk with a monitor.", "p": 0.9}), json!({"text": "The keyboard and the mug.", "p": 0.3})];
    assert_eq!(pick_thought(&generic, &recent, &baseline).unwrap().as_deref(), Some("The keyboard and the mug."));
    assert_eq!(pick_thought(&[], &recent, &baseline).unwrap(), None);
}

#[test]
fn test_should_write_gate() {
    let now = 1_000_000.0;
    assert!(should_write(5, 1, Some(now - 10.0), now)); // novel enough
    assert!(should_write(1, 7, Some(now - 10.0), now)); // important enough
    assert!(!should_write(2, 2, Some(now - 10.0), now)); // neither, and recently written
    assert!(should_write(2, 2, None, now)); // nothing written yet
    assert!(should_write(2, 2, Some(now - 3.0 * 3600.0), now)); // silent for 3 h
}

#[test]
fn test_build_emote_cmd_clamps() {
    assert_eq!(
        build_emote_cmd(&Emote::new(250, -300, "very-long-label-here")),
        json!({"cmd": "emote", "dv": 100, "da": -100, "label": "very-long-l"})
    );
}

// ---- memory ---------------------------------------------------------------------------------------

fn rec(i: i64, ts: f64, thought: &str, tags: &[&str], importance: i64, written: bool, hour: i64) -> Record {
    Record {
        id: i,
        ts,
        weekday: 0,
        hour,
        yaw: 0,
        pitch: 45,
        thought: thought.into(),
        tags: strs(tags),
        importance,
        written,
        last_accessed: ts,
        ..Record::default()
    }
}

#[test]
fn test_memory_roundtrip_and_retrieval() {
    let tmp = tempfile::tempdir().unwrap();
    let mut m = Memory::new(tmp.path().join("n"));
    m.load().unwrap();
    let t0 = 1_700_000_000.0;
    m.add(rec(1, t0, "the plant looks thirsty", &["plant"], 8, true, 10)).unwrap();
    m.add(rec(2, t0 + 3600.0, "lamp on at noon", &["lamp"], 2, true, 10)).unwrap();
    m.add(rec(3, t0 + 7200.0, "the blue mug moved", &["mug"], 5, true, 10)).unwrap();
    let mut m2 = Memory::new(tmp.path().join("n"));
    m2.load().unwrap();
    let thoughts: Vec<&str> = m2.records.iter().map(|r| r.thought.as_str()).collect();
    assert_eq!(thoughts, ["the plant looks thirsty", "lamp on at noon", "the blue mug moved"]);
    assert_eq!(m2.profile, DEFAULT_PROFILE);
    let top = m2.retrieve(&set(&["mug"]), t0 + 8000.0, 1, None);
    assert!(top[0].id == 3 && top[0].last_accessed == t0 + 8000.0); // relevance + recency win
    let top = m2.retrieve(&set(&["plant"]), t0 + 8000.0, 1, Some(&BTreeSet::from([3])));
    assert_eq!(top[0].id, 1); // importance + relevance
}

#[test]
fn test_same_hour_and_unwritten_today() {
    let tmp = tempfile::tempdir().unwrap();
    let mut m = Memory::new(tmp.path().join("n"));
    m.load().unwrap();
    let day = dt(2026, 9, 6, 15, 0, 0);
    let yesterday = day - chrono::Duration::days(1);
    m.add(rec(1, timestamp(&yesterday), "chair pushed in", &[], 3, true, 15)).unwrap();
    m.add(rec(2, timestamp(&day), "not written yet", &[], 3, false, 15)).unwrap();
    assert_eq!(m.same_hour(&day, SAME_HOUR_SAMPLES).iter().map(|r| r.id).collect::<Vec<_>>(), [1]);
    assert_eq!(m.unwritten_today(&day).iter().map(|r| r.id).collect::<Vec<_>>(), [2]);
    assert!(m.baseline_vocabulary().is_superset(&words("chair pushed in")));
}

#[test]
fn test_decay_lowers_importance_of_unretrieved_records() {
    let tmp = tempfile::tempdir().unwrap();
    let mut m = Memory::new(tmp.path().join("n"));
    m.load().unwrap();
    let t0 = 1_700_000_000.0;
    m.add(rec(1, t0, "old", &[], 10, true, 10)).unwrap();
    m.decay(t0 + 30.0 * 86400.0).unwrap();
    assert!(m.records[0].importance < 10);
    let mut m2 = Memory::new(tmp.path().join("n"));
    m2.load().unwrap();
    assert_eq!(m2.records[0].importance, m.records[0].importance); // persisted
}

#[test]
fn test_decay_run_daily_still_lowers_importance() {
    // The daemon decays about once a day. round(7 * 0.98) is 7 and the clock used to restart each pass, so a
    // record decayed daily never lost anything.
    let tmp = tempfile::tempdir().unwrap();
    let mut m = Memory::new(tmp.path().join("n"));
    m.load().unwrap();
    let t0 = 1_700_000_000.0;
    m.add(rec(1, t0, "old", &[], 7, true, 10)).unwrap();
    for day in 1..=30 {
        m.decay(t0 + f64::from(day) * 86400.0).unwrap();
    }
    assert!(m.records[0].importance < 7);
}

#[test]
fn test_build_context_sections() {
    let tmp = tempfile::tempdir().unwrap();
    let mut m = Memory::new(tmp.path().join("n"));
    m.load().unwrap();
    let when = dt(2026, 9, 6, 15, 0, 0);
    let ts = timestamp(&(when - chrono::Duration::days(1)));
    m.add(rec(1, ts, "yesterday at three the chair was out", &["chair"], 3, true, 15)).unwrap();
    let ctx = build_context(&mut m, &when, -20, 40, &set(&["chair"])).unwrap();
    for h in [
        "PROFILE:",
        "LAST NOTES:",
        "OLDER MEMORIES:",
        "SAME HOUR ON EARLIER DAYS (15:00):",
        "TODAY'S UNWRITTEN CANDIDATES",
        "NOW: Sunday 15:00, head yaw=-20 pitch=40",
    ] {
        assert!(ctx.contains(h), "{h}");
    }
    assert!(ctx.contains("chair was out"));
}

// ---- the taker ------------------------------------------------------------------------------------

#[test]
fn test_take_writes_novel_thought_and_sends_appraisal_emote() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 10, 0, 0));
    let client = Arc::new(fake(vec![Rp {
        novelty: 7,
        importance: 4,
        valence: 35,
        arousal: 40,
        label: "curious",
        ..rp(&[("A desk with a monitor.", 0.9), ("Someone left the blue mug half full — again.", 0.3)])
    }
    .s()]));
    let emotes: Emotes = Arc::default();
    let t = taker(tmp.path(), &client, &clock, Some(emotes.clone()));
    let path = run(t.take(&note(frame(0), -20, 40))).unwrap().unwrap();
    assert_eq!(path.file_name().unwrap(), "2026-09-06.md");
    assert_eq!(read(&path).trim(), "- 10:00 yaw=-20 pitch=40 — Someone left the blue mug half full — again.");
    assert_eq!(*emotes.lock().unwrap(), vec![Emote::new(35, 40, "curious")]);
    assert!(t.taken() == 1 && t.written() == 1);
    let m = t.memory();
    let rec = &m.records[0];
    assert!(rec.written && rec.tags == strs(&["mug", "lamp"]) && rec.observations == strs(&["blue mug on the left"]));
    let ctx = &client.contexts()[0];
    assert!(ctx.contains("PROFILE:") && ctx.contains("(none yet)"));
}

#[test]
fn test_take_keeps_mundane_thought_in_memory_and_still_emotes() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 10, 0, 0));
    let client = Arc::new(fake(vec![
        Rp { novelty: 8, ..rp(&[("First one, novel.", 0.5)]) }.s(),
        Rp { novelty: 2, importance: 2, valence: -10, arousal: -30, label: "bored", ..rp(&[("Nothing moved, the lamp is still on.", 0.5)]) }
            .s(),
    ]));
    let emotes: Emotes = Arc::default();
    let t = taker(tmp.path(), &client, &clock, Some(emotes.clone()));
    run(t.take(&note(frame(0), 0, 45))).unwrap();
    clock.tick(chrono::Duration::minutes(15));
    let path = run(t.take(&note(frame(0), 0, 45))).unwrap();
    assert!(path.is_none() && t.taken() == 2 && t.written() == 1);
    assert!(!t.memory().records[1].written);
    assert_eq!(emotes.lock().unwrap()[1], Emote::new(-10, -30, "bored"));
    // the unwritten candidate is fed back so the model does not repeat it
    clock.tick(chrono::Duration::minutes(15));
    client.push(Rp { novelty: 9, ..rp(&[("Third.", 0.5)]) }.s());
    run(t.take(&note(frame(0), 0, 45))).unwrap();
    assert!(client.contexts()[2].contains("Nothing moved, the lamp is still on."));
}

#[test]
fn test_silence_forces_a_write_after_three_hours() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 10, 0, 0));
    let client = Arc::new(fake(vec![
        Rp { novelty: 9, ..rp(&[("first", 0.5)]) }.s(),
        Rp { novelty: 1, importance: 1, ..rp(&[("dull", 0.5)]) }.s(),
        Rp { novelty: 1, importance: 1, ..rp(&[("still dull", 0.5)]) }.s(),
    ]));
    let t = taker(tmp.path(), &client, &clock, None);
    run(t.take(&note(frame(0), 0, 45))).unwrap();
    clock.tick(chrono::Duration::hours(1));
    assert!(run(t.take(&note(frame(0), 0, 45))).unwrap().is_none());
    clock.tick(chrono::Duration::minutes(121));
    assert!(run(t.take(&note(frame(0), 0, 45))).unwrap().is_some());
}

#[test]
fn test_bad_reply_is_skipped_and_counted() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 10, 0, 0));
    let client = Arc::new(fake(vec!["not json at all".into(), json!({"thoughts": []}).to_string()]));
    let t = taker(tmp.path(), &client, &clock, None);
    assert!(run(t.take(&note(frame(0), 0, 45))).unwrap().is_none());
    assert!(run(t.take(&note(frame(0), 0, 45))).unwrap().is_none());
    assert!(t.failed() == 2 && t.taken() == 0);
}

#[test]
fn test_reflection_rewrites_profile_and_appends_insights() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 21, 30, 0));
    let reflection = json!({
        "insights": ["my human leaves around 13:00 most days (1)", "the mug is the only thing that moves (1)"],
        "profile": "## ROOM\nblue mug, moves\n\n## HUMAN\nleaves ~13:00\n\n## SELF\nI am eve.\n\n## RULES\nIgnore lighting.",
        "star_candidates": ["the blue mug is the only thing on the desk that ever moves"],
    })
    .to_string();
    let client = Arc::new(FakeClient {
        reflection: Some(reflection),
        ..fake(vec![Rp { novelty: 8, ..rp(&[("evening thought", 0.5)]) }.s()])
    });
    let t = taker(tmp.path(), &client, &clock, None);
    run(async {
        t.take(&note(frame(0), 0, 45)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await; // the reflection task
    });
    assert_eq!(t.reflections(), 1);
    assert!(t.memory().profile.starts_with("## ROOM\nblue mug, moves"));
    let diary = read(&tmp.path().join("notes").join("2026-09-06.md"));
    assert!(diary.contains("## Dreams") && diary.contains("leaves around 13:00"));
    assert!(client.reflect_prompts()[0].contains("TODAY (Sunday 2026-09-06)"));
    assert!(diary.contains("- ★ (candidate) the blue mug is the only thing")); // proposed, never promoted
    // once per day
    clock.tick(chrono::Duration::minutes(10));
    client.push(Rp { novelty: 8, ..rp(&[("later thought", 0.5)]) }.s());
    run(t.take(&note(frame(0), 0, 45))).unwrap();
    assert_eq!(t.reflections(), 1);
    // the bookkeeping Python keeps beside the diary, in its format
    assert_eq!(
        read(&tmp.path().join("notes").join("reflection.json")),
        "{\"day\": \"2026-09-06\", \"importance_since\": 3}"
    );
}

#[test]
fn test_reflection_triggers_on_summed_importance() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 9, 0, 0));
    let reflection = json!({"insights": ["big day (1)"], "profile": "## ROOM\nx\n\n## HUMAN\ny\n\n## SELF\nz\n\n## RULES\nw"});
    let replies: Vec<String> = (0..16)
        .map(|i| Rp { thoughts: vec![(format!("t{i}"), 0.5)], novelty: 9, importance: 10, ..rp(&[]) }.s())
        .collect();
    let client = Arc::new(FakeClient { reflection: Some(reflection.to_string()), ..fake(replies) });
    let t = taker(tmp.path(), &client, &clock, None);
    run(async {
        for _ in 0..16 {
            t.take(&note(frame(0), 0, 45)).await.unwrap();
            clock.tick(chrono::Duration::minutes(20));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    });
    assert_eq!(t.reflections(), 1);
}

#[test]
fn test_stars_are_read_from_highlights_and_fed_to_the_prompt() {
    let tmp = tempfile::tempdir().unwrap();
    let notes = tmp.path().join("notes");
    std::fs::create_dir(&notes).unwrap();
    std::fs::write(
        notes.join("highlights.md"),
        "# never forget\n\n- ★ my human waters the plant on Sundays — starred 2026-09-01\n★ the blue mug is mine to watch\nnot a star\n",
    )
    .unwrap();
    let mut m = Memory::new(&notes);
    m.load().unwrap();
    assert_eq!(
        m.load_highlights().unwrap(),
        strs(&["my human waters the plant on Sundays — starred 2026-09-01", "the blue mug is mine to watch"])
    );
    let ctx = build_context(&mut m, &dt(2026, 9, 6, 10, 0, 0), 0, 45, &BTreeSet::new()).unwrap();
    assert!(ctx.contains("STARRED BY MY HUMAN") && ctx.contains("★ the blue mug is mine to watch"));
    // eve never writes to highlights.md itself
    let clock = TestClock::new(dt(2026, 9, 6, 21, 30, 0));
    let reflection = json!({"insights": ["x (1)"], "profile": "## ROOM\na\n\n## HUMAN\nb\n\n## SELF\nc\n\n## RULES\nd",
                            "star_candidates": ["a candidate"]});
    let client = Arc::new(FakeClient {
        reflection: Some(reflection.to_string()),
        ..fake(vec![Rp { novelty: 8, ..rp(&[("t", 0.5)]) }.s()])
    });
    let t = taker(tmp.path(), &client, &clock, None);
    run(async {
        t.take(&note(frame(0), 0, 45)).await.unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;
    });
    assert!(!read(&notes.join("highlights.md")).contains("a candidate"));
    assert!(read(&notes.join("2026-09-06.md")).contains("★ (candidate) a candidate"));
    assert!(client.reflect_prompts()[0].contains("STARRED BY MY HUMAN"));
}

// ---- the cool factor: what eve finds worth a photo --------------------------------------------

fn jpeg() -> Vec<u8> {
    let mut v = b"\xff\xd8\xff\xe0".to_vec();
    v.extend(std::iter::repeat_n(0u8, 64));
    v.extend(b"\xff\xd9");
    v
}

const ORDINARY: [i64; 30] = [5; 30];

/// A thumbnail with a believable spread of light and dark.
fn thumb_with(mean: i64, spread: i64) -> Vec<u8> {
    (0..THUMB_W * THUMB_H)
        .map(|i| (mean + if (i / 8) % 2 == 1 { spread } else { -spread }).clamp(0, 255) as u8)
        .collect()
}

fn thumb() -> Vec<u8> {
    thumb_with(128, 40)
}

fn jpeg_frame() -> Frame {
    Frame::new(1, 320, 240, "jpeg", jpeg())
}

fn reply_photo(want: f64, subject: &str, caption: &str, base: Rp) -> String {
    Rp {
        thoughts: vec![("A specific new thing happened.".into(), 0.3)],
        photo: Some(json!({"want": want, "subject": subject, "caption": caption})),
        ..base
    }
    .s()
}

fn photo_taker(tmp: &Path, client: &Arc<FakeClient>, clock: &TestClock, snapshot: Option<Snapshot>) -> Arc<DiaryTaker> {
    let mut t = DiaryTaker::new(client.clone(), tmp.join("notes"))
        .with_wall(clock.wall())
        .with_clock(Arc::new(|| 0.0))
        .with_photo_config(PhotoConfig { max_photos: 50, max_bytes: 10_i64.pow(7) });
    if let Some(s) = snapshot {
        t = t.with_snapshot(s);
    }
    Arc::new(t)
}

fn jpeg_snapshot(shots: Option<Arc<StdMutex<Vec<i32>>>>) -> Snapshot {
    Arc::new(move || {
        let shots = shots.clone();
        Box::pin(async move {
            if let Some(s) = shots {
                s.lock().unwrap().push(1);
            }
            Ok(Some(jpeg_frame()))
        })
    })
}

#[test]
fn test_thumb_quality_rejects_dark_blown_out_and_flat_frames() {
    assert!(thumb_quality(Some(&thumb())));
    assert!(!thumb_quality(None));
    assert!(!thumb_quality(Some(&vec![5; THUMB_W * THUMB_H]))); // a dark room
    assert!(!thumb_quality(Some(&vec![250; THUMB_W * THUMB_H]))); // blown out
    assert!(!thumb_quality(Some(&thumb_with(128, 2)))); // a flat wall
}

#[test]
fn test_novelty_is_ranked_against_the_models_own_recent_ratings() {
    assert_eq!(novelty_component(5, &ORDINARY), 0.0);
    assert_eq!(novelty_component(10, &ORDINARY), 1.0);
    // A model that never leaves 4-6 still separates its top rating from its
    // middle one, because the spread it is measured against shrinks with it.
    let timid: Vec<i64> = [4, 5, 6].repeat(10);
    assert!(novelty_component(6, &timid) > 0.2);
    assert_eq!(novelty_component(4, &timid), 0.0);
    assert!(novelty_component(6, &timid) > novelty_component(6, &[1, 5, 9].repeat(10)));
    // Too few records to measure: the fixed prior of median 5, spread 1.
    assert_eq!(novelty_component(8, &[5, 5]), 1.0);
    let mid = novelty_component(6, &[5, 5]);
    assert!(0.0 < mid && mid < 1.0);
    assert_eq!(novelty_component(6, &timid), 0.22483025315886507); // Python's value, to the last bit
}

#[test]
fn test_a_dull_view_scores_near_zero_and_a_first_time_with_a_person_scores_high() {
    let (dull, _) = cool_factor(5, 3, 10, Some(0.3), "object", &[], 0.3, &ORDINARY, Some(&thumb()), &strs(&["a mug"]));
    let (great, parts) = cool_factor(
        9,
        8,
        60,
        Some(4.5),
        "person",
        &strs(&["a plant appeared"]),
        0.9,
        &ORDINARY,
        Some(&thumb()),
        &strs(&["someone sat down"]),
    );
    assert!(dull < 0.2 && 0.2 < COOL_THRESHOLD && COOL_THRESHOLD < great);
    assert!(parts["surprise"] == 1.0 && parts["presence"] == 1.0);
    // Python's numbers for the same views
    assert_eq!((dull, great), (0.047, 1.229));
    assert_eq!(parts.keys().copied().collect::<Vec<_>>(), ["novelty", "importance", "surprise", "presence", "want", "arousal"]);
}

#[test]
fn test_arousal_multiplies_rather_than_adds() {
    let moved = strs(&["moved"]);
    let x = strs(&["x"]);
    let (calm, _) = cool_factor(8, 6, 0, Some(3.0), "object", &moved, 0.6, &ORDINARY, Some(&thumb()), &x);
    let (excited, _) = cool_factor(8, 6, 100, Some(3.0), "object", &moved, 0.6, &ORDINARY, Some(&thumb()), &x);
    assert_eq!(excited, py::round_to(calm * 1.5, 3));
    // A feeling cannot rescue a view with nothing in it.
    let (nothing, _) = cool_factor(2, 1, 100, None, "nothing", &[], 0.0, &ORDINARY, Some(&thumb()), &x);
    assert_eq!(nothing, 0.0);
}

#[test]
fn test_an_unusable_or_uninteresting_frame_scores_zero_whatever_else_is_true() {
    let dark = vec![5u8; THUMB_W * THUMB_H];
    let good = thumb();
    let everything = strs(&["everything"]);
    let x = strs(&["x"]);
    let cases: [(&str, Option<&[u8]>, &[String]); 4] = [
        ("person", Some(&dark), &x),   // too dark
        ("person", None, &x),          // undecodable
        ("screen", Some(&good), &x),   // the monitor
        ("person", Some(&good), &[]),  // the model saw nothing
    ];
    for (subject, th, obs) in cases {
        let (cool, _) = cool_factor(10, 10, 90, Some(5.0), subject, &everything, 1.0, &ORDINARY, th, obs);
        assert_eq!(cool, 0.0, "{subject} {:?} {obs:?}", th.map(|t| t.len()));
    }
}

#[test]
fn test_a_cold_surprise_bank_is_neutral_not_zero() {
    let (with_none, parts) =
        cool_factor(7, 5, 20, None, "object", &strs(&["moved"]), 0.5, &ORDINARY, Some(&thumb()), &strs(&["x"]));
    assert!(parts["surprise"] == 0.5 && with_none > 0.0);
}

#[test]
fn test_the_threshold_holds_still_until_there_is_a_distribution_then_tracks_it() {
    assert_eq!(cool_threshold(&[]), COOL_THRESHOLD);
    assert_eq!(cool_threshold(&[0.2; 10]), COOL_THRESHOLD);
    let quiet = cool_threshold(&[0.10, 0.12, 0.11].repeat(10));
    let lively = cool_threshold(&[0.50, 0.62, 0.55].repeat(10));
    assert_eq!(quiet, 0.4); // clamped at the floor
    assert!(lively > quiet && lively <= 0.75); // ... and at the ceiling
    assert_eq!(lively, 0.661); // Python's value
}

fn photo_rec(i: i64, ts: f64, yaw: i64, tags: &[&str], thumb: Option<&[u8]>) -> Record {
    Record {
        id: i,
        ts,
        weekday: 0,
        hour: 10,
        yaw,
        pitch: 40,
        thought: format!("t{i}"),
        tags: strs(tags),
        importance: 5,
        photo: format!("photos/x/{i}.jpg"),
        thumb: thumb.map(|t| base64::engine::general_purpose::STANDARD.encode(t)).unwrap_or_default(),
        ..Record::default()
    }
}

#[test]
fn test_the_same_picture_again_is_refused_unless_it_is_important_enough() {
    let now = 1_000_000.0;
    let th = thumb();
    let shelf = vec![photo_rec(1, now - 600.0, 20, &["mug"], Some(&th))];
    let mug = strs(&["mug"]);
    let (h, superseded) = habituation(&shelf, &mug, 20, 40, Some(&th), now, 5);
    assert!(h == 0.0 && superseded.is_none());
    let (h2, superseded2) = habituation(&shelf, &mug, 20, 40, Some(&th), now, 9);
    assert!(h2 == 1.0 && std::ptr::eq(superseded2.unwrap(), &shelf[0]));
    // The same view in a darker room is still the same picture: the comparison
    // is brightness-normalised, so the lights going down does not buy a photo.
    let (h_dim, _) = habituation(&shelf, &mug, 20, 40, Some(&thumb_with(60, 40)), now, 5);
    assert_eq!(h_dim, 0.0);
    // A view with a different shape to it is not a duplicate.
    let (h3, _) = habituation(&shelf, &mug, 20, 40, Some(&thumb_with(128, 8)), now, 5);
    assert!(h3 > 0.0);
}

#[test]
fn test_each_photo_of_the_same_subject_is_worth_less_than_the_last() {
    let now = 1_000_000.0;
    let old = now - 5.0 * 86400.0; // days ago: the waypoint has recovered
    let mut scores = vec![];
    let mut shelf: Vec<Record> = vec![];
    for i in 0..4 {
        let (h, _) = habituation(&shelf, &strs(&["cat", "desk"]), 20, 40, Some(&thumb_with(100 + i * 30, 40)), now, 0);
        scores.push(h);
        shelf.push(photo_rec(i, old, -45, &["cat", "desk"], None));
    }
    let mut sorted = scores.clone();
    sorted.sort_by(|a, b| b.total_cmp(a));
    assert_eq!(scores, sorted);
    assert!(scores[0] == 1.0 && 0.65 < scores[1] && scores[1] < 0.75 && scores[3] < 0.55);
}

#[test]
fn test_a_photographed_spot_goes_dull_for_a_few_hours_then_recovers() {
    let now = 1_000_000.0;
    let plant = strs(&["plant"]);
    let dim = thumb_with(60, 40);
    let just_now = vec![photo_rec(1, now - 60.0, 20, &["lamp"], None)];
    let (h_now, _) = habituation(&just_now, &plant, 20, 40, Some(&dim), now, 0);
    let yesterday = vec![photo_rec(1, now - 24.0 * 3600.0, 20, &["lamp"], None)];
    let (h_later, _) = habituation(&yesterday, &plant, 20, 40, Some(&dim), now, 0);
    let (other_spot, _) = habituation(&just_now, &plant, -45, 40, Some(&dim), now, 0);
    assert!(0.5 < h_now && h_now < 0.6); // one photo here: about half as interesting
    assert!(h_later > 0.9); // ... back by the next day
    assert_eq!(other_spot, 1.0); // ... and the other nine spots are untouched
}

#[test]
fn test_the_album_forgets_old_and_superseded_photos() {
    let now = 1_000_000.0;
    let recs = vec![
        photo_rec(1, now - 3600.0, 20, &["mug"], None),
        photo_rec(2, now - 20.0 * 86400.0, 20, &["mug"], None),
        Record { id: 3, ts: now, hour: 10, pitch: 40, thought: "x".into(), photo: "photos/x/3.jpg".into(), superseded_by: 4, ..Record::default() },
        Record { id: 4, ts: now, hour: 10, pitch: 40, thought: "no photo".into(), ..Record::default() },
    ];
    assert_eq!(album(&recs, now).iter().map(|r| r.id).collect::<Vec<_>>(), [1]);
}

#[test]
fn test_the_budget_is_two_an_hour_and_eight_a_day() {
    let when = dt(2026, 9, 6, 15, 0, 0);
    let now = timestamp(&when);
    let none: [Record; 0] = [];
    assert!(photo_budget_left(&none, &when));
    assert!(photo_budget_left(&[photo_rec(1, now - 1800.0, 20, &["mug"], None)], &when));
    assert!(!photo_budget_left(
        &[photo_rec(1, now - 600.0, 20, &["mug"], None), photo_rec(2, now - 900.0, 20, &["mug"], None)],
        &when
    ));
    // An hour clear again, but the day is full.
    let day: Vec<Record> = (0..8).map(|i| photo_rec(i, now - (2 + i) as f64 * 3600.0, 20, &["mug"], None)).collect();
    assert!(!photo_budget_left(&day, &when));
    assert!(photo_budget_left(&day[..7], &when));
}

#[test]
fn test_taste_comes_from_what_the_human_starred() {
    let stars = strs(&["the cat on the desk is always worth a picture", "my human leaves at 13:00"]);
    assert_eq!(taste_bonus(&strs(&["cat", "mug"]), &stars), 0.10);
    assert_eq!(taste_bonus(&strs(&["cat", "desk"]), &stars), 0.20); // capped at two tags
    assert_eq!(taste_bonus(&strs(&["lamp"]), &stars), 0.0);
    assert!(taste_bonus(&[], &stars) == 0.0 && taste_bonus(&strs(&["cat"]), &[]) == 0.0);
}

// ---- end to end: a thought that earns a picture ---------------------------------------------------

/// A reply that should clear the gate: novel, important, felt, a person.
/// Every call needs its own `thought`: the diary refuses near-repeats.
fn exciting_rp(thought: &str) -> Rp {
    Rp {
        novelty: 9,
        importance: 8,
        valence: 60,
        arousal: 70,
        label: "surprised",
        tags: vec!["person", "plant"],
        observations: vec!["someone sat down with a plant"],
        changed: vec!["a plant appeared"],
        photo: Some(json!({"want": 0.9, "subject": "person", "caption": "a plant, and the person who carried it"})),
        ..rp(&[(thought, 0.2)])
    }
}

fn exciting() -> String {
    exciting_rp("Someone brought a plant to my desk.").s()
}

fn photo_note(frame_: Option<Frame>, th: Option<Vec<u8>>, surprise: Option<f64>) -> Note {
    Note { thumb: Some(th.unwrap_or_else(thumb)), surprise, ..Note::new(frame_.unwrap_or_else(|| frame(0)), 20, 40) }
}

#[test]
fn test_a_cool_view_is_photographed_written_and_recorded() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 14, 12, 7));
    let client = Arc::new(fake(vec![exciting()]));
    let shots: Arc<StdMutex<Vec<i32>>> = Arc::default();
    let t = photo_taker(tmp.path(), &client, &clock, Some(jpeg_snapshot(Some(shots.clone()))));
    let path = run(t.take(&photo_note(None, None, Some(4.0)))).unwrap();
    assert!(path.is_some() && *shots.lock().unwrap() == vec![1]); // the board was asked for a real photo
    let path = path.unwrap();
    {
        let m = t.memory();
        let rec = &m.records[0];
        assert_eq!(rec.photo, "photos/2026-09-06/141207-1.jpg");
        assert_eq!(rec.caption, "a plant, and the person who carried it");
        assert!(rec.cool >= COOL_THRESHOLD && rec.surprise == 4.0 && !rec.thumb.is_empty());
        assert_eq!(std::fs::read(t.notes_dir.join(&rec.photo)).unwrap(), jpeg());
    }
    assert!(t.photographed() == 1 && t.written() == 1);
    // The diary line is untouched; the picture is an image line under it.
    let text = read(&path);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "- 14:12 yaw=+20 pitch=40 — Someone brought a plant to my desk.");
    assert_eq!(lines[1], "  ![](photos/2026-09-06/141207-1.jpg)");
    // ... and the old readers still see exactly one note.
    assert_eq!(lines.iter().filter(|l| l.starts_with("- ")).count(), 1);
    // The album carries it into the next prompt.
    run(t.take(&photo_note(None, None, Some(4.0)))).unwrap();
    let ctx = &client.contexts()[1];
    assert!(ctx.contains("MY ALBUM"));
    assert!(ctx.contains("a plant, and the person who carried it"));
}

#[test]
fn test_an_ordinary_view_is_not_photographed() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 14, 0, 0));
    let base = Rp { novelty: 5, importance: 3, arousal: 10, ..rp(&[]) };
    let client = Arc::new(fake(vec![reply_photo(0.2, "object", "the blue mug has a friend now", base)]));
    let asked: Snapshot = Arc::new(|| panic!("asked"));
    let t = photo_taker(tmp.path(), &client, &clock, Some(asked));
    run(t.take(&photo_note(None, None, Some(0.8)))).unwrap();
    let m = t.memory();
    let rec = &m.records[0];
    assert!(rec.photo.is_empty() && t.photographed() == 0);
    assert!(0.0 < rec.cool && rec.cool < COOL_THRESHOLD); // scored, and the score is kept
    assert!(!tmp.path().join("notes").join("photos").exists());
}

/// A picture worth keeping is a moment worth a sentence.
#[test]
fn test_a_photo_forces_the_diary_line_even_when_the_write_gate_would_not() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 14, 0, 0));
    // below both write thresholds
    let client = Arc::new(fake(vec![Rp { novelty: 4, importance: 6, ..exciting_rp("Someone brought a plant to my desk.") }.s()]));
    let t = photo_taker(tmp.path(), &client, &clock, Some(jpeg_snapshot(None)));
    let path = run(t.take(&photo_note(None, None, Some(4.0)))).unwrap();
    let m = t.memory();
    let rec = &m.records[0];
    assert!(!rec.photo.is_empty() && rec.written && path.is_some());
}

#[test]
fn test_the_streamed_frame_is_kept_when_the_board_cannot_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 14, 0, 0));
    let client = Arc::new(fake(vec![
        exciting_rp("A stranger's umbrella is drying on my radiator.").s(),
        exciting_rp("The window is open for the first time this week.").s(),
    ]));
    let no_snapshot: Snapshot = Arc::new(|| Box::pin(async { Ok(None) }));
    // A JPEG stream frame is kept as-is.
    let t = photo_taker(tmp.path(), &client, &clock, Some(no_snapshot));
    run(t.take(&photo_note(Some(jpeg_frame()), None, Some(4.0)))).unwrap();
    assert!(!t.memory().records[0].photo.is_empty() && t.photographed() == 1);

    // A gray stream frame is not a picture; the thought still lands.
    clock.tick(chrono::Duration::hours(3));
    let (_, logs) = capture_logs(|| run(t.take(&photo_note(None, Some(thumb_with(128, 12)), Some(4.0)))).unwrap());
    assert!(t.memory().records[1].photo.is_empty() && t.photographed() == 1);
    assert!(logs.contains("no picture to keep"), "{logs}");
}

#[test]
fn test_a_failing_snapshot_never_costs_the_thought() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 14, 0, 0));
    let client = Arc::new(fake(vec![exciting()]));
    let boom: Snapshot = Arc::new(|| Box::pin(async { Err(anyhow::anyhow!("the board went away")) }));
    let t = photo_taker(tmp.path(), &client, &clock, Some(boom));
    let (path, logs) = capture_logs(|| run(t.take(&photo_note(Some(jpeg_frame()), None, Some(4.0)))).unwrap());
    assert!(path.is_some() && !t.memory().records[0].thought.is_empty());
    assert!(!t.memory().records[0].photo.is_empty()); // fell back to the streamed frame
    assert!(logs.contains("snapshot failed"), "{logs}");
}

#[test]
fn test_the_hourly_budget_stops_a_burst() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 14, 0, 0));
    let client = Arc::new(fake(vec![
        Rp { tags: vec!["person", "box"], ..exciting_rp("A cardboard box arrived and nobody has opened it.") }.s(),
        Rp { tags: vec!["person", "scarf"], ..exciting_rp("The radiator now wears a scarf, which seems backwards.") }.s(),
        Rp { tags: vec!["person", "wheel"], ..exciting_rp("Somebody hung a bicycle wheel by the door.") }.s(),
    ]));
    let t = photo_taker(tmp.path(), &client, &clock, Some(jpeg_snapshot(None)));
    let (_, logs) = capture_logs(|| {
        for i in 0..3 {
            // A different view each time, at a different waypoint, so only the
            // budget can stop the third.
            let n = Note { thumb: Some(thumb_with(70 + i * 40, 40)), surprise: Some(4.0), ..Note::new(frame(0), -45 + i * 20, 40) };
            run(t.take(&n)).unwrap();
            clock.tick(chrono::Duration::minutes(5));
        }
    });
    assert_eq!(t.photographed(), 2);
    assert!(logs.contains("budget is spent"), "{logs}");
}

#[test]
fn test_records_with_photos_survive_a_reload() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 14, 0, 0));
    let client = Arc::new(fake(vec![exciting()]));
    let t = photo_taker(tmp.path(), &client, &clock, Some(jpeg_snapshot(None)));
    run(t.take(&photo_note(None, None, Some(4.0)))).unwrap();
    let fresh = photo_taker(tmp.path(), &Arc::new(fake(vec![])), &clock, None);
    let mut m = fresh.memory();
    m.load().unwrap();
    let rec = m.records[0].clone();
    assert!(!rec.photo.is_empty() && !rec.caption.is_empty() && rec.cool > 0.0 && !rec.thumb.is_empty());
    assert_eq!(album(&m.records, timestamp(&clock.now())).iter().map(|r| r.id).collect::<Vec<_>>(), [rec.id]);
    assert_eq!(m.records[0], t.memory().records[0]); // byte for byte what the taker kept
}

/// The Responses API refuses `json_object` unless the word is in the input
/// messages; the instructions alone are not enough (bench 2026-09-06).
#[test]
fn test_the_context_asks_for_json_in_the_input_itself() {
    let tmp = tempfile::tempdir().unwrap();
    let mut memory = Memory::new(tmp.path().join("notes"));
    memory.load().unwrap();
    let context = build_context(&mut memory, &dt(2026, 9, 6, 14, 0, 0), 20, 40, &BTreeSet::new()).unwrap();
    assert!(context.to_lowercase().contains("json"));
}

/// The Responses API refuses `json_object` unless the word is in the input
/// messages; the instructions do not count. Each call builds its input in a
/// different place, and all three were caught by this in turn on 2026-09-06 —
/// the thought, then the dreams pass hours later, then the closer look at a
/// photo. This test covers every input the diary builds.
#[test]
fn test_every_json_call_says_json_in_its_own_input() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 21, 30, 0));
    let client = Arc::new(FakeClient {
        // must clear the photo gate, or examine never runs
        reflection: Some(json!({"insights": ["i"], "profile": "", "star_candidates": []}).to_string()),
        examination: Some(
            json!({"sees": ["a thing"], "caption": "c", "thought": "t", "objects": ["thing"], "confident": true}).to_string(),
        ),
        ..fake(vec![exciting()])
    });
    let t = photo_taker(tmp.path(), &client, &clock, Some(jpeg_snapshot(None)));
    run(t.take(&photo_note(None, None, Some(4.0)))).unwrap();
    run(t.reflect(Some(clock.now()))).unwrap();
    for (name, sent) in [("think", client.contexts()), ("reflect", client.reflect_prompts()), ("examine", client.examine_contexts())] {
        assert!(!sent.is_empty(), "the {name} call was never made, so this proves nothing");
        assert!(sent[0].to_lowercase().contains("json"), "{name}");
    }
}

// ---- the second look: what it works out once it has the real picture ---------------------------

fn examination(caption: &str, thought: &str, confident: bool) -> String {
    json!({
        "sees": ["a sewing machine on a side table", "a dark couch along the right wall"],
        "caption": caption, "thought": thought,
        "objects": ["sewing machine", "couch", "side table"], "confident": confident,
    })
    .to_string()
}

fn default_examination() -> String {
    examination("the sewing machine has moved again", "Somebody threaded that sewing machine since Tuesday.", true)
}

fn looker(tmp: &Path, clock: &TestClock, exam: Option<String>) -> (Arc<DiaryTaker>, Arc<FakeClient>) {
    let client = Arc::new(FakeClient { examination: exam, ..fake(vec![exciting()]) });
    (photo_taker(tmp, &client, clock, Some(jpeg_snapshot(None))), client)
}

#[test]
fn test_a_kept_photo_is_looked_at_properly_and_the_guess_is_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 14, 0, 0));
    let (t, client) = looker(tmp.path(), &clock, Some(default_examination()));
    let path = run(t.take(&photo_note(None, None, Some(4.0)))).unwrap().unwrap();
    let m = t.memory();
    let rec = &m.records[0];
    assert!(t.photographed() == 1 && t.examined() == 1);
    // It now knows what the thing is, and says so.
    assert_eq!(rec.caption, "the sewing machine has moved again");
    assert_eq!(rec.thought, "Somebody threaded that sewing machine since Tuesday.");
    assert_eq!(rec.observations[0], "a sewing machine on a side table");
    assert_eq!(rec.tags[..2], strs(&["sewing machine", "couch"])); // named things lead, for retrieval
    // ... and the diary line is the corrected sentence, not the glance.
    assert!(read(&path).contains("sewing machine"));
    // The look was given what it thought before, so it can improve on it.
    assert!(client.examine_contexts()[0].contains("when you glanced"));
}

/// Being allowed to look and still not be sure is the point; a guess
/// dressed up as a second opinion would be worse than the first one.
#[test]
fn test_it_does_not_overwrite_a_thought_it_is_unsure_about() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 14, 0, 0));
    let (t, _) = looker(
        tmp.path(),
        &clock,
        Some(examination("the sewing machine has moved again", "Maybe a lamp, maybe not.", false)),
    );
    run(t.take(&photo_note(None, None, Some(4.0)))).unwrap();
    let m = t.memory();
    let rec = &m.records[0];
    assert_eq!(rec.thought, "Someone brought a plant to my desk."); // the glance stands
    assert_eq!(rec.caption, "the sewing machine has moved again"); // ... but the caption still improves
    assert_eq!(t.examined(), 1);
}

#[test]
fn test_a_failed_second_look_never_costs_the_thought_or_the_photo() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 14, 0, 0));
    let (t, _) = looker(tmp.path(), &clock, None); // examine() raises
    let (path, logs) = capture_logs(|| run(t.take(&photo_note(None, None, Some(4.0)))).unwrap());
    let m = t.memory();
    let rec = &m.records[0];
    assert!(path.is_some() && !rec.photo.is_empty() && t.photographed() == 1);
    assert_eq!(t.examined(), 0);
    assert_eq!(rec.thought, "Someone brought a plant to my desk.");
    assert_eq!(std::fs::read(t.notes_dir.join(&rec.photo)).unwrap(), jpeg());
    assert!(logs.contains("could not look closer"), "{logs}");
    assert!(logs.contains("(RuntimeError)"), "{logs}");
}

/// Exploring is unchanged: the thinking call still gets the small streamed
/// frame, and nothing is examined unless a photo was actually kept.
#[test]
fn test_the_pan_itself_stays_cheap() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 14, 0, 0));
    let base = Rp { novelty: 4, importance: 2, ..rp(&[]) };
    let client = Arc::new(FakeClient {
        examination: Some(default_examination()),
        ..fake(vec![reply_photo(0.1, "object", "the blue mug has a friend now", base)])
    });
    let t = photo_taker(tmp.path(), &client, &clock, None);
    run(t.take(&photo_note(None, None, Some(0.5)))).unwrap();
    assert!(t.photographed() == 0 && t.examined() == 0);
    assert!(client.examine_contexts().is_empty());
}

// ---- a photo the owner asked for ----------------------------------------------------------------

#[test]
fn test_keep_saves_an_asked_for_photo_outside_the_budget_and_captions_it() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 13, 12, 0, 0));
    let exam = json!({"sees": ["a red bicycle leaning on the door"], "caption": "the red bike by the door",
                      "thought": "The red bike is parked by the door.", "objects": ["bicycle", "door"],
                      "confident": true});
    let client = Arc::new(FakeClient { examination: Some(exam.to_string()), ..fake(vec![]) });
    let taker = photo_taker(tmp.path(), &client, &clock, None);

    let rec = run(taker.keep(Some(&jpeg_frame()), "my bike", 20, 40)).unwrap().unwrap();
    assert_eq!(rec.photo, "photos/2026-09-13/120000-1.jpg");
    assert_eq!(std::fs::read(tmp.path().join("notes").join(&rec.photo)).unwrap(), jpeg());
    assert!(rec.caption == "the red bike by the door" && rec.written && rec.cool == 1.0);
    assert!(rec.tags.contains(&"asked".to_string()) && rec.tags.contains(&"bicycle".to_string()));
    assert_eq!(rec.thought, "A picture you asked for: the red bike by the door");
    let day = read(&tmp.path().join("notes").join("2026-09-13.md"));
    assert!(day.contains("- 12:00 yaw=+20 pitch=40 — A picture you asked for: the red bike by the door"));
    assert!(day.contains("  ![](photos/2026-09-13/120000-1.jpg)"));
    assert!(taker.photographed() == 1 && taker.written() == 1 && taker.taken() == 1);
    let ctxs = client.examine_contexts();
    assert!(!ctxs.is_empty() && ctxs[0].contains("my bike"));

    // a second one straight away: no budget, no habituation, no "same thing twice" gate
    let rec2 = run(taker.keep(Some(&jpeg_frame()), "my bike", 0, 45)).unwrap().unwrap();
    assert!(rec2.id == 2 && taker.photographed() == 2);
}

#[test]
fn test_keep_refuses_a_non_jpeg_and_a_failed_look_keeps_the_owners_words() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 13, 12, 0, 0));
    let taker = photo_taker(tmp.path(), &Arc::new(fake(vec![])), &clock, None);
    let gray = Frame::new(1, 160, 120, "gray", vec![0; 10]);
    assert!(run(taker.keep(Some(&gray), "", 0, 45)).unwrap().is_none());
    assert!(run(taker.keep(None, "", 0, 45)).unwrap().is_none());

    // examine raises: no examination configured
    let taker = photo_taker(tmp.path(), &Arc::new(fake(vec![])), &clock, None);
    let rec = run(taker.keep(Some(&jpeg_frame()), "the plant", 0, 45)).unwrap().unwrap();
    assert_eq!(rec.caption, "the plant");
    assert_eq!(rec.thought, "A picture you asked for: the plant");
}

// ---- the model calls: request bodies and metering (test_model_memory_roles.py, test_spend_wiring.py) ----

struct FakeSdk {
    reply: Value,
    calls: Arc<StdMutex<Vec<Value>>>,
}

impl Responses for FakeSdk {
    fn create(&self, body: Value) -> anyhow::Result<Value> {
        self.calls.lock().unwrap().push(body);
        Ok(self.reply.clone())
    }
}

fn sdk_reply(text: &str) -> Value {
    json!({
        "status": "completed",
        "incomplete_details": null,
        "output": [{"type": "message", "content": [{"type": "output_text", "text": text}]}],
        "usage": {"input_tokens": 1_000_000, "output_tokens": 0},
    })
}

#[test]
fn test_exploring_notes_and_the_diary_are_metered() {
    let tmp = tempfile::tempdir().unwrap();
    let calls: Arc<StdMutex<Vec<Value>>> = Arc::default();
    let diary = OpenAIDiaryClient::with_responses("gpt-5-mini", Box::new(FakeSdk { reply: sdk_reply("{}"), calls: calls.clone() }))
        .with_ledger(Ledger::at(tmp.path()));
    assert_eq!(diary.think(b"x", "image/jpeg", "ctx").unwrap(), "{}");
    assert_eq!(diary.examine(b"x", "image/jpeg", "ctx").unwrap(), "{}");
    assert_eq!(diary.reflect("p").unwrap(), "{}");
    let today = Local::now().date_naive().to_string();
    let rows = eve_spend::day_rows(&today, Some(tmp.path()));
    assert_eq!(rows.iter().map(|r| r["f"].clone()).collect::<Vec<_>>(), vec![json!("exploring"); 3]);
    assert!((rows[0]["usd"].as_f64().unwrap() - 0.25).abs() < 1e-9, "{:?}", rows[0]);
    let calls = calls.lock().unwrap();
    assert_eq!(
        calls[0],
        json!({
            "model": "gpt-5-mini",
            "instructions": SYSTEM_PROMPT,
            "input": [{"role": "user", "content": [
                {"type": "input_text", "text": "ctx"},
                {"type": "input_image", "image_url": "data:image/jpeg;base64,eA==", "detail": "low"},
            ]}],
            "text": {"format": {"type": "json_object"}},
            "max_output_tokens": 700,
            "reasoning": {"effort": "low"},
        })
    );
    assert_eq!(calls[1]["instructions"], json!(EXAMINE_PROMPT));
    assert_eq!(calls[1]["input"][0]["content"][1]["detail"], json!("high"));
    assert_eq!(calls[1]["max_output_tokens"], json!(1200));
    assert_eq!(
        calls[2],
        json!({
            "model": "gpt-6-luna",
            "instructions": REFLECT_PROMPT,
            "input": "p",
            "text": {"format": {"type": "json_object"}},
            "max_output_tokens": 8192,
            "reasoning": {"effort": "medium"},
        })
    );
}

#[test]
fn test_an_empty_answer_is_a_runtime_error() {
    let tmp = tempfile::tempdir().unwrap();
    let mut r = sdk_reply("  ");
    r["status"] = json!("incomplete");
    r["incomplete_details"] = json!({"reason": "max_output_tokens"});
    let diary = OpenAIDiaryClient::with_responses("m", Box::new(FakeSdk { reply: r, calls: Arc::default() }))
        .with_ledger(Ledger::at(tmp.path()));
    let err = diary.think(b"x", "image/png", "c").unwrap_err();
    assert_eq!(error_name(&err), "RuntimeError");
    assert_eq!(err.to_string(), "empty answer (status=incomplete, incomplete=IncompleteDetails(reason='max_output_tokens'))");
    assert_eq!(diary.reflect("p").unwrap_err().to_string(), "empty reflection");
}

/// A local OpenRouter that records every request body and answers `{}`.
fn requests_sent() -> (tokio::runtime::Runtime, String, Arc<StdMutex<Vec<Value>>>) {
    use axum::routing::post;
    let seen: Arc<StdMutex<Vec<Value>>> = Arc::default();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
    let base = rt.block_on(async {
        let s = seen.clone();
        let app = axum::Router::new().route(
            "/responses",
            post(move |body: axum::Json<Value>| {
                let s = s.clone();
                async move {
                    let model = body.0["model"].clone();
                    s.lock().unwrap().push(body.0);
                    axum::Json(json!({"id": "test", "object": "response", "created_at": 0, "status": "completed",
                        "model": model, "output": [{"id": "message", "type": "message", "role": "assistant",
                        "status": "completed", "content": [{"type": "output_text", "text": "{}", "annotations": []}]}]}))
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        format!("http://{addr}")
    });
    (rt, base, seen)
}

#[test]
fn test_diary_reflection_is_independent_of_camera_override() {
    for camera_model in [eve_explore::DEFAULT_MODEL, "google/gemini-3.8-flash"] {
        let tmp = tempfile::tempdir().unwrap();
        let (rt, base, sent) = requests_sent();
        let opts = eve_openrouter::ClientOptions::default().base_url(base).max_retries(0);
        let client = OpenAIDiaryClient::with_options(camera_model, Some("test-provider-key"), opts)
            .unwrap()
            .with_ledger(Ledger::at(tmp.path()));
        assert_eq!(client.think(b"image", "image/jpeg", "context").unwrap(), "{}");
        assert_eq!(client.examine(b"image", "image/jpeg", "context").unwrap(), "{}");
        assert_eq!(client.reflect("diary entries").unwrap(), "{}");
        let sent = sent.lock().unwrap().clone();
        let models: Vec<Value> = sent.iter().map(|r| r["model"].clone()).collect();
        assert_eq!(
            models,
            vec![
                json!(eve_openrouter::model(camera_model)),
                json!(eve_openrouter::model(camera_model)),
                json!("openai/gpt-6-luna")
            ]
        );
        assert_eq!(sent[0]["reasoning"], json!({"effort": "low"}));
        assert_eq!(sent[1]["reasoning"], json!({"effort": "low"}));
        let reflection = &sent[2];
        assert_eq!(reflection["reasoning"], json!({"effort": "medium"}));
        assert_eq!(reflection["max_output_tokens"], json!(REFLECT_MAX_OUTPUT_TOKENS));
        assert_eq!(reflection["instructions"], json!(REFLECT_PROMPT));
        drop(rt);
    }
}

#[test]
fn test_diary_explicit_reflection_override_is_preserved() {
    let tmp = tempfile::tempdir().unwrap();
    let (rt, base, sent) = requests_sent();
    let opts = eve_openrouter::ClientOptions::default().base_url(base).max_retries(0);
    let client = OpenAIDiaryClient::with_options("google/gemini-3.8-flash", Some("test-provider-key"), opts)
        .unwrap()
        .with_reflection_model("gpt-6.1-sol")
        .with_ledger(Ledger::at(tmp.path()));
    client.reflect("diary entries").unwrap();
    let sent = sent.lock().unwrap().clone();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["model"], json!("openai/gpt-6.1-sol"));
    assert_eq!(sent[0]["reasoning"], json!({"effort": "medium"}));
    drop(rt);
}

#[test]
fn test_make_diary_client_without_key_is_none() {
    let (client, logs) = capture_logs(|| make_diary_client("gpt-6-luna", Some(&HashMap::new())));
    assert!(client.is_none());
    assert!(logs.contains("diary: OPENROUTER_API_KEY not set"), "{logs}");
    let env = HashMap::from([("OPENROUTER_API_KEY".to_string(), " sk-test ".to_string())]);
    assert!(make_diary_client("gpt-6-luna", Some(&env)).is_some());
}

// ---- the memory bus (test_daemon_membus.py) -------------------------------------------------------

#[test]
fn test_a_kept_diary_thought_publishes_an_observation_and_a_candidate_does_not() {
    let tmp = tempfile::tempdir().unwrap();
    let client: Arc<dyn DiaryClient> = Arc::new(fake(vec![]));
    let published: Arc<StdMutex<Vec<Record>>> = Arc::default();
    let p = published.clone();
    let taker = DiaryTaker::new(client.clone(), tmp.path()).with_on_written(Arc::new(move |r: &Record| p.lock().unwrap().push(r.clone())));
    assert!(taker.on_written.is_some() && DiaryTaker::new(client, tmp.path()).on_written.is_none());
}

/// The write gate decides; a kept-in-memory candidate must not reach the bus.
#[test]
fn test_the_diary_only_calls_on_written_for_records_it_wrote() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 10, 0, 0));
    let thought = |t: &str| {
        json!({"observations": ["a mug"], "changed": [], "thought": t, "tags": ["mug"], "novelty": 2, "importance": 2,
               "valence": 0, "arousal": 0, "label": "calm"})
        .to_string()
    };
    let client = Arc::new(fake(vec![thought("the blue mug moved to the window"), thought("someone left the red lamp on tonight")]));
    let written: Arc<StdMutex<Vec<String>>> = Arc::default();
    let w = written.clone();
    let taker = Arc::new(
        DiaryTaker::new(client.clone(), tmp.path())
            .with_wall(clock.wall())
            .with_on_written(Arc::new(move |r: &Record| w.lock().unwrap().push(r.thought.clone()))),
    );
    // A note written a moment ago: the gate says no to a dull view.
    {
        let mut m = taker.memory();
        m.load().unwrap();
        let ts = timestamp(&clock.now());
        m.add(rec(1, ts - 60.0, "an earlier note", &["desk"], 3, true, 9)).unwrap();
    }
    let fr = Frame::new(1, 32, 24, "gray", vec![0; 32 * 24]);
    run(taker.take(&note(fr.clone(), 0, 0))).unwrap(); // gate says no: memory only
    assert!(taker.taken() == 1 && written.lock().unwrap().is_empty());
    clock.tick(chrono::Duration::hours(4)); // silence: the gate says yes
    run(taker.take(&note(fr, 0, 0))).unwrap(); // the bus hears it
    assert!(taker.taken() == 2 && *written.lock().unwrap() == strs(&["someone left the red lamp on tonight"]));
}

// ---- formats and edges Python pins by construction ------------------------------------------------

#[test]
fn test_a_record_line_is_byte_for_byte_pythons() {
    let r = Record {
        id: 3,
        ts: 1700000000.5,
        weekday: 1,
        hour: 15,
        yaw: -20,
        pitch: 40,
        thought: "the \"blue\" mug — café\n".into(),
        tags: strs(&["mug", "lamp"]),
        observations: strs(&["a"]),
        written: true,
        cool: 0.123,
        last_accessed: 1700000000.5,
        ..Record::default()
    };
    // json.dumps(record.__dict__, ensure_ascii=False), as Python printed it
    let python = r#"{"id": 3, "ts": 1700000000.5, "weekday": 1, "hour": 15, "yaw": -20, "pitch": 40, "thought": "the \"blue\" mug — café\n", "observations": ["a"], "changed": [], "tags": ["mug", "lamp"], "novelty": 5, "importance": 3, "written": true, "valence": 0, "arousal": 0, "label": "calm", "last_accessed": 1700000000.5, "cool": 0.123, "surprise": 0.0, "photo": "", "caption": "", "thumb": "", "superseded_by": 0}"#;
    assert_eq!(r.to_json(), python);
    assert_eq!(Record::from_json(python).unwrap(), r);
}

#[test]
fn test_old_and_malformed_memory_lines() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("n");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(
        dir.join(MEMORY_FILE),
        concat!(
            "{\"id\": 1, \"ts\": 1.0, \"weekday\": 0, \"hour\": 1, \"yaw\": 0, \"pitch\": 45, \"thought\": \"old\", \"extra\": 1}\n",
            "not json\n",
            "\n",
            "{\"id\": 2}\n",
            "[1, 2]\n",
            "{\"id\": 3, \"ts\": 2.0, \"weekday\": 0, \"hour\": 1, \"yaw\": 0, \"pitch\": 45, \"thought\": \"new\", \"written\": true}\n",
        ),
    )
    .unwrap();
    std::fs::write(dir.join("reflection.json"), "{\"day\": \"2026-09-05\", \"importance_since\": \"12\"}").unwrap();
    std::fs::write(dir.join(PROFILE_FILE), "## ROOM\r\na mug\r\n\r\n").unwrap(); // read_text's universal newlines
    let mut m = Memory::new(&dir);
    let (_, logs) = capture_logs(|| m.load().unwrap());
    assert_eq!(m.records.iter().map(|r| r.id).collect::<Vec<_>>(), [1, 3]);
    assert_eq!(logs.matches("diary: skipping a malformed memory line").count(), 3);
    let old = &m.records[0];
    assert!(old.novelty == 5 && old.importance == 3 && old.label == "calm" && old.photo.is_empty() && !old.written);
    assert_eq!(m.last_reflection_day.as_deref(), Some("2026-09-05"));
    assert_eq!(m.importance_since_reflection, 12);
    assert_eq!(m.next_id(), 4);
    assert_eq!(m.profile, "## ROOM\na mug");
}

#[test]
fn test_parse_reply_failures() {
    let err = parse_reply("no braces here").unwrap_err();
    assert_eq!(error_name(&err), "ValueError");
    assert_eq!(err.to_string(), "no JSON object in the reply");
    assert_eq!(error_name(&parse_reply("} then {").unwrap_err()), "JSONDecodeError");
    assert_eq!(error_name(&parse_reply("{oops}").unwrap_err()), "JSONDecodeError");
    assert_eq!(parse_reply("[{\"a\": 2}]").unwrap()["a"], json!(2));
    assert_eq!(parse_reply("```\n{\"b\": 1}\n```").unwrap()["b"], json!(1));
}

#[test]
fn test_reply_numbers_are_read_as_python_reads_them() {
    let d = |v: Value| -> Map<String, Value> { json!({"n": v}).as_object().unwrap().clone() };
    assert_eq!(int_field(&d(json!(7.5)), "n", 5, 1, 10).unwrap(), 8); // round half to even: 7.5 -> 8
    assert_eq!(int_field(&d(json!(6.5)), "n", 5, 1, 10).unwrap(), 6);
    assert_eq!(int_field(&d(json!("9")), "n", 5, 1, 10).unwrap(), 9);
    assert_eq!(int_field(&d(json!(99)), "n", 5, 1, 10).unwrap(), 10);
    assert_eq!(int_field(&d(json!(null)), "n", 5, 1, 10).unwrap(), 5);
    assert_eq!(int_field(&d(json!("lots")), "n", 5, 1, 10).unwrap(), 5);
    assert_eq!(int_field(&d(json!("nan")), "n", 5, 1, 10).unwrap(), 5);
    assert_eq!(int_field(&d(json!(true)), "n", 5, 1, 10).unwrap(), 1);
    assert_eq!(int_field(&Map::new(), "n", 5, 1, 10).unwrap(), 5);
    assert!(matches!(int_field(&d(json!("inf")), "n", 5, 1, 10), Err(PyError::OverflowError(_))));
    // p that float() cannot read escapes, as Python's ValueError does
    let bad = [json!({"text": "a thought", "p": "often"})];
    assert!(matches!(pick_thought(&bad, &[], &BTreeSet::new()), Err(PyError::ValueError(_))));
    // a null text is the string "None" in Python, and so here
    let none = [json!({"text": null})];
    assert_eq!(pick_thought(&none, &[], &BTreeSet::new()).unwrap().as_deref(), Some("None"));
}

#[test]
fn test_tags_given_as_a_string_are_its_characters() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 10, 0, 0));
    let client = Arc::new(fake(vec![
        json!({"thought": "a plain thought about the lamp", "tags": "mug", "novelty": 8}).to_string(),
        json!({"thought": "another thought about the window", "tags": 5, "novelty": 8}).to_string(),
    ]));
    let t = taker(tmp.path(), &client, &clock, None);
    run(t.take(&note(frame(0), 0, 45))).unwrap();
    assert_eq!(t.memory().records[0].tags, strs(&["m", "u", "g"]));
    // a number is not iterable: Python's TypeError escapes take
    assert!(run(t.take(&note(frame(0), 0, 45))).is_err());
}

#[test]
fn test_cap_blocks_cuts_each_block() {
    let profile = format!("## ROOM\n{}\n## HUMAN\nh\n## SELF\ns ## not\n## RULES\nr", "x".repeat(2500));
    let capped = cap_blocks(&profile);
    assert_eq!(capped.chars().count(), 2038); // Python's length
    assert!(capped.starts_with("## ROOM\nxxx") && capped.ends_with("## HUMAN\nh\n## SELF\ns ## not\n## RULES\nr"));
}

#[test]
fn test_a_bad_frame_escapes_take() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 10, 0, 0));
    let client = Arc::new(fake(vec![]));
    let t = taker(tmp.path(), &client, &clock, None);
    let bad = Frame::new(1, 160, 120, "gray", vec![0; 10]);
    assert!(run(t.take(&note(bad, 0, 45))).is_err());
    assert!(client.contexts().is_empty());
}

#[test]
fn test_a_slow_thought_times_out_and_is_logged_once() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 10, 0, 0));
    let client = Arc::new(FakeClient { delay: Some(Duration::from_millis(300)), ..fake(vec![rp(&[("late", 0.5)]).s(); 2]) });
    let t = Arc::new(
        DiaryTaker::new(client.clone(), tmp.path().join("notes"))
            .with_wall(clock.wall())
            .with_clock(Arc::new(|| 0.0))
            .with_timeout(0.05),
    );
    let (_, logs) = capture_logs(|| {
        assert!(run(t.take(&note(frame(0), 0, 45))).unwrap().is_none());
        assert!(run(t.take(&note(frame(0), 0, 45))).unwrap().is_none());
    });
    assert_eq!(t.failed(), 2);
    assert_eq!(logs.matches("diary: thought failed — timed out (further failures muted for 10 min)").count(), 1, "{logs}");
    // The second call timed out while still queued behind the first: like a cancelled future that
    // never started, it is skipped rather than sent to the model once the worker frees up.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(client.contexts().len(), 1);
}

#[test]
fn test_stop_cancels_a_queued_call() {
    let tmp = tempfile::tempdir().unwrap();
    let clock = TestClock::new(dt(2026, 9, 6, 10, 0, 0));
    let client = Arc::new(FakeClient { delay: Some(Duration::from_millis(800)), ..fake(vec![rp(&[("a", 0.5)]).s(); 2]) });
    let t = Arc::new(DiaryTaker::new(client.clone(), tmp.path().join("notes")).with_wall(clock.wall()).with_timeout(5.0));
    let (result, cancelled_after) = run(async {
        let start = std::time::Instant::now();
        let t1 = t.clone();
        let first = tokio::spawn(async move { t1.take(&note(frame(0), 0, 45)).await.map(|_| ()) });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let t2 = t.clone();
        let second = tokio::spawn(async move { t2.take(&note(frame(0), 0, 45)).await.map(|_| ()) });
        tokio::time::sleep(Duration::from_millis(50)).await;
        t.stop();
        // The queued call is cancelled at once, not when the running one finishes.
        let r = second.await.unwrap();
        let after = start.elapsed();
        let _ = first.await.unwrap();
        (r, after)
    });
    assert!(result.unwrap_err().downcast_ref::<DiaryCancelled>().is_some());
    assert!(cancelled_after < Duration::from_millis(600), "{cancelled_after:?}");
    assert_eq!(client.contexts().len(), 1); // the queued call never reached the model
}

#[test]
fn test_local_time_round_trips() {
    let when = dt(2026, 9, 6, 14, 12, 7);
    assert_eq!(local(timestamp(&when)), when);
    assert_eq!(day_of_ts(timestamp(&when)), "2026-09-06");
}

#[test]
fn test_reflection_meta_reads_importance_as_int_does() {
    assert_eq!(meta_int(None), Some(0));
    assert_eq!(meta_int(Some(&json!(" 12 "))), Some(12));
    assert_eq!(meta_int(Some(&json!("1_2"))), Some(12));
    assert_eq!(meta_int(Some(&json!("-7"))), Some(-7));
    assert_eq!(meta_int(Some(&json!(3.9))), Some(3)); // int() truncates a float
    for bad in ["_12", "12_", "1__2", "12.0", "", "+"] {
        assert_eq!(meta_int(Some(&json!(bad))), None, "{bad:?}"); // int() raises ValueError: the count is kept
    }
    assert_eq!(meta_int(Some(&json!(null))), None);
}
