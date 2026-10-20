//! The daemon's side of memory (test_daemon_transcripts.py) and the daemon on its memory bus
//! (test_daemon_membus.py): the one-time move before any store opens, the nightly dream, close markers and a drain
//! at shutdown, a bus "remember" that becomes a transcript line and never a star, the lesson and observation
//! events, the opt-in sinks and the recall service. Nothing touches the owner's real memory folder: every test
//! lends its own environment and its own temp dir.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::{Duration as ChronoDuration, TimeZone};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_daemon::run::{
    build_memory_with, close_conversations, drain_jobs, dream_loop, lesson_event, observation_event, recall, remember,
    start_sinks, stop_sinks, today_blocks, voice_memory_with, MemoryOpeners, TodayBuild,
};
use eve_daemon::seams::{BoardLink, Job, Obj};
use eve_daemon::{Daemon, DaemonParts};
use eve_memory::Memory;
use eve_memory_bus::{BusConfig, MemoryBus};
use eve_records::RecallConfig;
use eve_transcripts::transcripts::{BACKEND_CHARS, BUS, CHANNELS, TALK, VOICE_CHARS};
use eve_transcripts::{Append, Dt, TranscriptConfig, Transcripts};

// ---- helpers ---------------------------------------------------------------------------------------------------

fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
    pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

fn cfg(tmp: &Path) -> RecallConfig {
    RecallConfig::new(tmp.join("memory"), tmp.join("notes"))
}

/// A memory over `tmp` with no mem0 index, its transcripts on `wall` (None: the local clock).
fn memory_on(tmp: &Path, wall: Option<eve_transcripts::transcripts::Wall>) -> Arc<Memory> {
    let cfg = cfg(tmp);
    let t = Transcripts::new(TranscriptConfig::new(true, cfg.transcripts_dir()), Some(cfg.archive_dir()), wall);
    let opts = eve_memory::MemoryOptions { environ: Some(HashMap::new()), ..Default::default() };
    Arc::new(Memory::with_options(cfg, Arc::new(t), None, opts))
}

fn memory(tmp: &Path) -> Arc<Memory> {
    memory_on(tmp, None)
}

/// Noon today, so "20 minutes ago" is always today's transcript day (the day starts at 04:00).
fn noon_wall() -> eve_transcripts::transcripts::Wall {
    let now = chrono::Local::now();
    let noon = chrono::Local
        .with_ymd_and_hms(
            chrono::Datelike::year(&now),
            chrono::Datelike::month(&now),
            chrono::Datelike::day(&now),
            12,
            0,
            0,
        )
        .single()
        .expect("noon exists");
    let noon: Dt = noon.fixed_offset();
    Arc::new(move || noon)
}

fn obj(v: Value) -> Obj {
    match v {
        Value::Object(m) => m,
        _ => Map::new(),
    }
}

/// The log lines written on this thread while `f` runs.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.lock()).into_owned()
    }
}

fn capture() -> (Captured, tracing::subscriber::DefaultGuard) {
    let cap = Captured::default();
    let writer = cap.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    (cap, tracing::subscriber::set_default(subscriber))
}

/// Every line of every day of `t`.
fn all_lines(t: &Transcripts) -> Vec<Arc<eve_transcripts::Line>> {
    t.days().iter().flat_map(|d| t.lines(d)).collect()
}

fn s(line: &eve_transcripts::Line, key: &str) -> String {
    line.get(key).and_then(Value::as_str).unwrap_or_default().to_string()
}

// ---- the move, before anything opens a store ------------------------------------------------------------------

fn recording_openers(order: Arc<Mutex<Vec<&'static str>>>) -> MemoryOpeners {
    let (o1, o2, o3, o4) = (order.clone(), order.clone(), order.clone(), order);
    MemoryOpeners {
        migrate: Arc::new(move |_cfg, _store, _mem0| {
            o1.lock().push("migrate");
            Ok(json!({}))
        }),
        ensure_repo: Arc::new(move |_repo| {
            o2.lock().push("ensure_repo");
            true
        }),
        transcripts: Arc::new(move |config, archive| {
            o3.lock().push("transcripts");
            Transcripts::new(config, archive, None)
        }),
        mem0: Arc::new(move |_cfg, _environ| {
            o4.lock().push("mem0");
            None
        }),
    }
}

#[test]
fn test_with_memory_on_the_move_runs_before_any_store_opens() {
    let tmp = tempfile::tempdir().unwrap();
    let environ = env(&[("EVE_MEMORY", "1"), ("EVE_MEMORY_DIR", tmp.path().join("memory").to_str().unwrap())]);
    let order = Arc::new(Mutex::new(Vec::new()));
    let memory = build_memory_with(&cfg(tmp.path()), Some(&environ), &recording_openers(order.clone()));
    let memory = memory.expect("a memory");
    assert_eq!(*order.lock(), ["migrate", "ensure_repo", "transcripts", "mem0"]);
    assert!(memory.transcripts.enabled);
    assert_eq!(memory.transcripts.archive, Some(cfg(tmp.path()).archive_dir()));
    assert!(memory.index.is_none());
}

#[test]
fn test_with_memory_off_nothing_moves_and_nothing_opens() {
    let tmp = tempfile::tempdir().unwrap();
    let order = Arc::new(Mutex::new(Vec::new()));
    let environ = HashMap::new();
    assert!(build_memory_with(&cfg(tmp.path()), Some(&environ), &recording_openers(order.clone())).is_none());
    assert!(order.lock().is_empty());
    assert!(!tmp.path().join("memory").exists());
}

#[test]
fn test_the_real_move_puts_the_old_store_under_the_new_root() {
    // End to end with the real migrate: the old records land in records/ as a sealed repository of its own, the
    // old notes in transcripts/meetings, and everything else in the archive.
    if which_git().is_none() {
        eprintln!("git is not installed");
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let legacy = tmp.path().join("debrief");
    std::fs::create_dir_all(legacy.join("records")).unwrap();
    std::fs::write(legacy.join("records/coffee.md"), "---\nid: coffee\ntype: preference\n---\n- flat white\n").unwrap();
    std::fs::create_dir_all(legacy.join("notes/2026-09-20")).unwrap();
    std::fs::write(legacy.join("notes/2026-09-20/1400-standup.md"), "# Standup\n").unwrap();
    std::fs::create_dir_all(legacy.join("sessions/2026-09-20")).unwrap();
    std::fs::write(legacy.join("sessions/2026-09-20/0900-a1.md"), "# A chat\n").unwrap();
    let environ = env(&[
        ("EVE_MEMORY", "1"),
        ("EVE_MEM0", "0"),
        ("EVE_MEMORY_DIR", tmp.path().join("memory").to_str().unwrap()),
        ("EVE_DEBRIEF_DIR", legacy.to_str().unwrap()),
    ]);
    // The old mem0 home is always ~/.config/eve-bridge/mem0: the real migrate is lent a private one, so this test
    // can never move the owner's real mem0 folder (the Python test lost it once, 2026-09-23).
    let private_mem0 = tmp.path().join("home/.config/eve-bridge/mem0");
    let openers = MemoryOpeners {
        migrate: Arc::new(move |cfg, store, _owner_mem0| eve_memory::migrate(cfg, store, &private_mem0)),
        ..MemoryOpeners::default()
    };
    let c = cfg(tmp.path());
    let memory = build_memory_with(&c, Some(&environ), &openers).expect("a memory");
    assert!(memory.index.is_none());
    assert!(c.records_dir().join("coffee.md").is_file() && c.records_dir().join(".git").is_dir());
    assert!(c.transcripts_dir().join("meetings/2026-09-20/1400-standup.md").is_file());
    assert!(c.archive_dir().join("sessions/2026-09-20/0900-a1.md").is_file());
    assert!(c.store.join(eve_memory::MIGRATED_FILE).is_file());
    // the archive is searched: the old note is found by the same search the brains use
    let found = memory
        .transcripts
        .search_with("chat", &eve_transcripts::Search { days_back: 3650, ..Default::default() });
    let chs: Vec<&str> = found["hits"].as_array().unwrap().iter().map(|h| h["ch"].as_str().unwrap()).collect();
    assert_eq!(chs, ["note"]);
}

fn which_git() -> Option<PathBuf> {
    std::env::var_os("PATH")?.to_str()?.split(':').map(|d| Path::new(d).join("git")).find(|p| p.is_file())
}

#[test]
fn test_run_opens_memory_before_the_ipc_server() {
    // The IPC server can write meeting notes into the memory folder, so the move must come first. (Python patched
    // the two calls to record their order; here the order is read off run()'s source, as Python's own
    // inspect-based tests read daemon.py.)
    let source = include_str!("../src/run.rs");
    let run = &source[source.find("pub async fn run(").unwrap()..];
    let memory = run.find("self.open_memory().await").expect("run opens memory");
    let ipc = run.find("ipc.start().await").expect("run starts the IPC server");
    assert!(memory < ipc);
}

// ---- the dream, and the loops that are gone ---------------------------------------------------------------------

struct NoCalls;

impl eve_records::DreamClient for NoCalls {
    fn reconcile(&self, _body: &str) -> anyhow::Result<String> {
        anyhow::bail!("no night was due")
    }
    fn consolidate(&self, _body: &str) -> anyhow::Result<String> {
        anyhow::bail!("no night was due")
    }
}

#[tokio::test]
async fn test_the_dream_loop_starts_in_the_background_with_memory_and_a_client() {
    let tmp = tempfile::tempdir().unwrap();
    let memory = memory(tmp.path());
    let (tx, rx) = tokio::sync::watch::channel(false);
    let client: Arc<dyn eve_records::DreamClient + Send + Sync> = Arc::new(NoCalls);
    let fut = dream_loop(Some(&memory), &memory.cfg, Some(client.clone()), None, rx.clone()).expect("a dream");
    let task = Job::spawn("memory-dream", fut);
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(!task.done()); // it waits for the night, or for the shutdown
    tx.send_replace(true);
    tokio::time::timeout(Duration::from_secs(1), task.join()).await.expect("it ended on the shutdown event");
    assert!(!task.cancelled()); // it ended by itself

    // no client (no key): no dream; memory off: no dream
    assert!(dream_loop(Some(&memory), &memory.cfg, None, None, rx.clone()).is_none());
    assert!(dream_loop(None, &memory.cfg, Some(client), None, rx).is_none());
}

fn crate_sources() -> String {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut all = String::new();
    for entry in std::fs::read_dir(dir).unwrap() {
        all.push_str(&std::fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    all
}

#[test]
fn test_no_curate_reconcile_or_mem0_loop_is_left() {
    let source = include_str!("../src/run.rs");
    let run = &source[source.find("pub async fn run(").unwrap()..source.find("fn start_learning").unwrap()];
    assert!(run.contains("self.start_dream()")); // the control: the one loop that remains
    let daemon = crate_sources();
    for gone in ["curate_loop", "make_reconciler", "records-reconcile", "mem0-ingest", "chat-memory"] {
        assert!(!daemon.contains(gone), "{gone}");
    }
    let records = Path::new(env!("CARGO_MANIFEST_DIR")).join("../eve-records/src");
    for entry in std::fs::read_dir(records).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap();
        assert!(!text.contains("fn make_reconciler") && !text.contains("struct Reconciler"));
    }
}

// ---- a voice conversation -------------------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_converse_lends_the_memory_and_its_blocks_to_the_session() {
    // (the part of _converse that is _voice_memory's: the conv and the two today blocks)
    let tmp = tempfile::tempdir().unwrap();
    let memory = memory_on(tmp.path(), Some(noon_wall()));
    let t = &memory.transcripts;
    let yesterday = t.now() - ChronoDuration::days(1);
    let at = |now: Dt| Append { now: Some(now), ..Default::default() };
    assert!(t.append_with("telegram", "t-1726000000000-abcd", "owner", "say", "the plant needs water", at(yesterday)));
    assert!(t.append_with(
        "telegram",
        "t-1726000000001-abcd",
        "owner",
        "say",
        "the kettle is new",
        at(t.now() - ChronoDuration::minutes(20)),
    ));
    let (conv, today, backend_today) = voice_memory_with(Some(memory.clone()), 5.0, today_blocks()).await;
    assert!(conv.starts_with("v-"));
    assert!(today.contains("kettle") && backend_today.contains("kettle"));
    assert!(!today.contains("plant")); // yesterday is not today
    assert!(today.chars().count() <= VOICE_CHARS);
    assert!(backend_today.chars().count() <= BACKEND_CHARS);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_a_slow_today_build_opens_the_conversation_without_it() {
    let tmp = tempfile::tempdir().unwrap();
    let memory = memory(tmp.path());
    assert!(memory.transcripts.append("telegram", "t-1726000000001-abcd", "owner", "say", "the kettle is new"));
    let real = today_blocks();
    let slow: TodayBuild = Arc::new(move |m: &Memory| {
        std::thread::sleep(Duration::from_millis(300));
        real(m)
    });
    let started = Instant::now();
    let (conv, today, backend_today) = voice_memory_with(Some(memory), eve_daemon::TODAY_BUILD_SECS, slow).await;
    let took = started.elapsed();
    assert!(conv.starts_with("v-"));
    assert_eq!((today.as_str(), backend_today.as_str()), ("", ""));
    assert!(took < Duration::from_millis(250), "{took:?}"); // it did not wait for the slow build
}

#[tokio::test]
async fn test_with_memory_off_the_brains_get_nothing_and_prompts_are_unchanged() {
    let (conv, today, backend_today) = voice_memory_with(None, eve_daemon::TODAY_BUILD_SECS, today_blocks()).await;
    assert_eq!((conv.as_str(), today.as_str(), backend_today.as_str()), ("", "", ""));
    let vc = eve_brain::voice_agent::VoiceConfig::default();
    let plain = eve_brain::voice_agent::session_config(&vc, "", None, "", "", "", "", None, None);
    assert_eq!(eve_brain::voice_agent::session_config(&vc, "", None, "", "", &today, &backend_today, None, None), plain);
}

// ---- shutdown -------------------------------------------------------------------------------------------------

#[test]
fn test_shutdown_closes_open_conversations_once() {
    let tmp = tempfile::tempdir().unwrap();
    let memory = memory(tmp.path());
    let t = &memory.transcripts;
    let (voice, tg, done, silent) = (t.new_conv("voice"), t.new_conv("telegram"), t.new_conv("voice"), t.new_conv("telegram"));
    assert!(t.append("voice", &voice, "owner", "say", "one"));
    assert!(t.append("telegram", &tg, "owner", "say", "two"));
    assert!(t.append("voice", &done, "owner", "say", "three") && t.close("voice", &done, None));
    let closes = |conv: &str| t.conv_lines(conv).iter().filter(|ln| s(ln, "kind") == "close").count();
    let pair = |ch: &str, conv: &str| (ch.to_string(), conv.to_string());

    assert_eq!(close_conversations(t, &[pair("voice", &voice), pair("telegram", &tg)]), 2);
    assert!(closes(&voice) == 1 && closes(&tg) == 1);
    // already closed, and one that wrote nothing
    assert_eq!(close_conversations(t, &[pair("voice", &done), pair("telegram", &silent)]), 0);
    assert!(closes(&done) == 1 && t.conv_lines(&silent).is_empty());
    // a second shutdown pass closes nothing twice
    assert_eq!(close_conversations(t, &[pair("voice", &voice), pair("telegram", "")]), 0);
    assert_eq!(closes(&voice), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shutdown_awaits_background_work_then_cuts_off_the_rest() {
    let finished: Arc<Mutex<Vec<&str>>> = Arc::default();
    let (f1, f2) = (finished.clone(), finished.clone());
    let quick = Job::spawn("quick", async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        f1.lock().push("quick");
    });
    let stuck = Job::spawn("stuck", async move {
        tokio::time::sleep(Duration::from_secs(30)).await;
        f2.lock().push("stuck");
    });
    let started = Instant::now();
    drain_jobs(vec![quick.clone(), stuck.clone()], 0.2).await;
    let took = started.elapsed();
    assert_eq!(*finished.lock(), ["quick"]);
    assert!(quick.done() && !quick.cancelled());
    assert!(stuck.cancelled() && stuck.done());
    assert!(took < Duration::from_secs(1));
}

#[test]
fn test_the_drain_budget_is_twelve_seconds() {
    assert_eq!(eve_daemon::DRAIN_SECS, 12.0);
    assert_eq!(eve_daemon::TODAY_BUILD_SECS, 0.05);
    assert_eq!(eve_daemon::CLOSE_MARKERS_SECS, 3.0);
}

// ---- a remember on the bus ------------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_bus_remember_is_a_transcript_line_and_never_a_star() {
    let tmp = tempfile::tempdir().unwrap();
    let memory = memory(tmp.path());
    let bus = MemoryBus::new();
    let m = memory.clone();
    bus.subscribe("/eve/memory/remember", move |_t: &str, msg: &Obj| remember(Some(&m), msg));
    let (cap, _guard) = capture();
    bus.publish("/eve/memory/remember", obj(json!({"text": "The green mug lives on the left shelf.", "title": "Mug"})));
    bus.publish("/eve/memory/remember", obj(json!({"text": "   "}))); // nothing to keep
    let t = &memory.transcripts;
    let lines = all_lines(t);
    let said: Vec<_> = lines.iter().filter(|ln| s(ln, "kind") != "close").collect();
    assert_eq!(said.len(), 1);
    let line = said[0];
    assert_eq!(
        (s(line, "ch"), s(line, "who"), s(line, "kind"), s(line, "tool")),
        ("bus".into(), "system".into(), "tool".into(), "remember".into())
    );
    assert_eq!(s(line, "text"), "Mug: The green mug lives on the left shelf.");
    let conv = s(line, "conv");
    assert_eq!(lines.iter().filter(|ln| s(ln, "kind") == "close" && s(ln, "conv") == conv).count(), 1);
    assert!(!memory.cfg.records_dir().join(eve_records::records::STARRED_FILE).exists()); // never a star
    assert!(eve_records::records::stars(&memory.cfg, 100).is_empty());
    // for the dream to judge, never the owner talking: not in today's block, not the last talk
    assert!(t.day_text(&t.day_of(t.now()), 100_000, false).contains("left shelf"));
    assert!(eve_transcripts::render_line(line, "text").contains("(sent)"));
    assert_eq!(t.today_block(10000), "");
    assert!(Transcripts::new(t.config.clone(), None, None).last_turn_at().is_none());
    let logs = cap.text();
    assert!(!logs.contains("left shelf"), "{logs}"); // logs never carry the words
    assert!(logs.contains("a transcript line"), "{logs}");
}

#[test]
fn test_the_bus_channel_is_known_to_the_transcripts() {
    assert!(BUS == "bus" && CHANNELS.contains(&BUS));
    assert_eq!(TALK, ["voice", "telegram"]);
}

#[test]
fn test_recent_conversation_never_shows_a_bus_line() {
    let tmp = tempfile::tempdir().unwrap();
    let memory = memory(tmp.path());
    let t = &memory.transcripts;
    let since = t.now() - ChronoDuration::minutes(5);
    assert!(t.append("telegram", &t.new_conv("telegram"), "owner", "say", "typed on the phone"));
    let bus_conv = t.new_conv("bus");
    assert!(t.append_with("bus", &bus_conv, "system", "tool", "sent over the bus", Append { tool: "remember".into(), ..Default::default() }));
    let rows: Vec<String> = t.lines_since(since, "voice").iter().map(|ln| s(ln, "text")).collect();
    assert_eq!(rows, ["typed on the phone"]); // the control is there; the bus line is not
    let hits = t.search("bus");
    let chs: Vec<&str> = hits["hits"].as_array().unwrap().iter().map(|h| h["ch"].as_str().unwrap()).collect();
    assert_eq!(chs, ["bus"]); // but search still finds it
}

// ---- configured() (test_daemon_membus.py) ---------------------------------------------------------------------

#[test]
fn test_everything_is_off_by_default() {
    assert_eq!(eve_memory_bus::configured(Some(&HashMap::new())), BusConfig::default());
}

#[test]
fn test_env_flags_and_mirror_default() {
    let cfg = eve_memory_bus::configured(Some(&env(&[
        ("EVE_ROSBRIDGE", "1"),
        ("EVE_ROSBRIDGE_PORT", "9999"),
        ("EVE_ROSBRIDGE_HOST", "0.0.0.0"),
        ("EVE_CLAUDE_MEM", "true"),
        ("EVE_CLAUDE_MEM_URL", "http://127.0.0.1:37701"),
    ])));
    assert_eq!(
        cfg,
        BusConfig {
            rosbridge: true,
            rosbridge_host: "0.0.0.0".into(),
            rosbridge_port: 9999,
            claude_mem: true,
            claude_mem_url: Some("http://127.0.0.1:37701".into()),
            mirror: true,
        }
    );
    assert!(!eve_memory_bus::configured(Some(&env(&[("EVE_CLAUDE_MEM", "on"), ("EVE_CLAUDE_MEM_MIRROR", "off")]))).mirror);
    assert_eq!(
        eve_memory_bus::configured(Some(&env(&[("EVE_ROSBRIDGE", "1"), ("EVE_ROSBRIDGE_PORT", "lots")]))).rosbridge_port,
        9090
    );
}

// ---- emits ----------------------------------------------------------------------------------------------------

/// `_Store.summary` of test_daemon_membus.py's lesson: what `store.summary` keeps.
fn lesson_summary() -> Obj {
    obj(json!({"id": "abc", "topic": "Fractions", "level": "Grade 4", "mode": "learn",
               "problem": "What is 1/2 + 1/4?", "stage": "working", "updated": 1.0, "demo": true, "revision": 3}))
}

#[test]
fn test_a_lesson_notice_publishes_the_lesson_without_the_learner_words() {
    let e = lesson_event(Some(&lesson_summary()), "", "working", "What do the bottoms need to be?", 5.0);
    assert!(e["action"] == "notice" && e["stage"] == "working");
    assert!(e["topic"] == "Fractions" && e["level"] == "Grade 4" && e["mode"] == "learn");
    assert!(e["lesson_id"] == "abc" && e["feedback"] == "What do the bottoms need to be?");
    for forbidden in ["ideas", "spoken", "strokes", "board_image", "problem"] {
        assert!(!e.contains_key(forbidden), "{forbidden}");
    }
    let keys: Vec<&str> = e.keys().map(String::as_str).collect();
    assert_eq!(keys, ["action", "stage", "topic", "level", "mode", "lesson_id", "feedback", "time"]);
    assert!(e["time"].is_f64());
    // the lesson's own stage stands in for a notice that has none
    assert_eq!(lesson_event(Some(&lesson_summary()), "hint", "", "  a\n  hint ", 5.0)["stage"], "working");
    assert_eq!(lesson_event(Some(&lesson_summary()), "hint", "", "  a\n  hint ", 5.0)["feedback"], "a hint");
}

#[test]
fn test_a_lesson_notice_without_a_learning_server_still_publishes_a_bare_event() {
    let e = lesson_event(None, "", "error", "Try again.", 5.0);
    assert!(e["action"] == "notice" && e["stage"] == "error" && e["topic"] == "" && e["feedback"] == "Try again.");
}

fn record() -> eve_diary::Record {
    eve_diary::Record {
        id: 1,
        ts: 1700000000.0,
        weekday: 1,
        hour: 15,
        yaw: 0,
        pitch: 0,
        thought: "the chair is pushed in".into(),
        observations: vec!["chair pushed in".into()],
        changed: vec!["chair".into()],
        tags: vec!["chair".into(), "desk".into()],
        novelty: 7,
        importance: 8,
        written: true,
        valence: 0,
        arousal: 0,
        label: String::new(),
        last_accessed: 0.0,
        cool: 0.0,
        surprise: 0.0,
        photo: "photos/x.jpg".into(),
        caption: String::new(),
        thumb: "AAAA".into(),
        superseded_by: 0,
    }
}

#[test]
fn test_a_kept_diary_thought_publishes_an_observation_and_a_candidate_does_not() {
    let e = observation_event(&record(), 99.0);
    assert_eq!(
        Value::Object(e.clone()),
        json!({"thought": "the chair is pushed in", "observations": ["chair pushed in"], "changed": ["chair"],
               "tags": ["chair", "desk"], "importance": 8, "novelty": 7, "time": 1700000000.0})
    );
    assert!(!e.contains_key("photo") && !e.contains_key("thumb"));
    // a record with no time is stamped now
    let mut timeless = record();
    timeless.ts = 0.0;
    assert_eq!(observation_event(&timeless, 99.0)["time"], 99.0);
}

#[test]
fn test_nothing_said_in_a_conversation_is_published() {
    // The conversation topic is gone (owner, 2026-09-23): what was said stays in the memory folder.
    let topics: Vec<&str> = eve_memory_bus::TOPICS.iter().map(|(t, _)| *t).collect();
    assert!(!topics.contains(&"/eve/memory/conversation"));
    assert!(!crate_sources().contains("publish_conversation"));
    assert!(topics.contains(&"/eve/memory/observation")); // the control: the diary topic is still there
}

// ---- sinks ----------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_with_the_flags_off_no_sink_starts_and_the_boot_line_says_so() {
    let bus = MemoryBus::new();
    let (cap, _guard) = capture();
    let sinks = start_sinks(&bus, &BusConfig::default()).await;
    assert!(sinks.rosbridge.is_none() && sinks.claude_mem_sink.is_none() && sinks.claude_mem_mirror.is_none());
    assert!(cap.text().contains("memory bus: rosbridge off, claude-mem off, mirror off"), "{}", cap.text());
    stop_sinks(sinks).await;
}

/// A local URL nothing listens on.
fn closed_port_url() -> String {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    format!("http://127.0.0.1:{port}")
}

#[tokio::test]
async fn test_with_the_flags_on_the_sinks_start_and_stop() {
    let bus = MemoryBus::new();
    let url = closed_port_url();
    let cfg = BusConfig {
        rosbridge: true,
        rosbridge_port: 0,
        claude_mem: true,
        claude_mem_url: Some(url.clone()),
        mirror: true,
        ..BusConfig::default()
    };
    let (cap, _guard) = capture();
    let sinks = start_sinks(&bus, &cfg).await;
    let server = sinks.rosbridge.clone().expect("rosbridge started");
    assert!(server.is_serving() && server.port() != 0);
    assert!(sinks.claude_mem_sink.is_some() && sinks.claude_mem_mirror.is_some());
    let line = format!("memory bus: rosbridge ws://127.0.0.1:{}, claude-mem {url}, mirror on", server.port());
    assert!(cap.text().contains(&line), "{}", cap.text());
    stop_sinks(sinks).await;
    assert!(!server.is_serving());
}

#[tokio::test]
async fn test_a_sink_that_cannot_start_is_logged_and_the_daemon_goes_on() {
    let busy = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = busy.local_addr().unwrap().port();
    let bus = MemoryBus::new();
    let cfg = BusConfig {
        rosbridge: true,
        rosbridge_port: i64::from(port),
        claude_mem: true,
        claude_mem_url: Some(closed_port_url()),
        mirror: false,
        ..BusConfig::default()
    };
    let (cap, _guard) = capture();
    let sinks = start_sinks(&bus, &cfg).await;
    assert!(sinks.rosbridge.is_none() && sinks.claude_mem_sink.is_some());
    assert!(cap.text().contains("rosbridge did not start (OSError: [Errno"), "{}", cap.text());
    stop_sinks(sinks).await;
    drop(busy);
}

// ---- recall and remember --------------------------------------------------------------------------------------

#[tokio::test]
async fn test_recall_says_claude_mem_is_off_when_it_is() {
    let bus = MemoryBus::new();
    bus.register_service("/eve/memory/recall", |args: Obj| async move { Ok(recall(false, &args, None).await) });
    let out = bus.call_service("/eve/memory/recall", obj(json!({"query": "mug"}))).await.unwrap();
    assert_eq!(Value::Object(out), json!({"results": [], "error": "claude-mem is off (EVE_CLAUDE_MEM=1)"}));
}

/// A claude-mem worker that answers one search with row #1 titled by the query, and records the paths asked.
async fn fake_worker() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let paths: Arc<Mutex<Vec<String>>> = Arc::default();
    let seen = paths.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { return };
            let seen = seen.clone();
            tokio::spawn(async move {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = vec![0u8; 8192];
                let mut got = Vec::new();
                while !got.windows(4).any(|w| w == b"\r\n\r\n") {
                    let Ok(n) = sock.read(&mut buf).await else { return };
                    if n == 0 {
                        return;
                    }
                    got.extend_from_slice(&buf[..n]);
                }
                let head = String::from_utf8_lossy(&got).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                seen.lock().push(path.clone());
                let body = if path.starts_with("/api/search?") {
                    let table = "| ID | Time | T | Title |\n|----|------|---|-------|\n| #1 | now | o | mug |\n";
                    json!({"content": [{"type": "text", "text": table}]}).to_string()
                } else {
                    json!({"narrative": "the green mug"}).to_string()
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    (url, paths)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_recall_searches_claude_mem_when_it_is_on() {
    let (url, paths) = fake_worker().await;
    let out = recall(true, &obj(json!({"query": "mug", "limit": "2"})), Some(&url)).await;
    assert_eq!(Value::Object(out), json!({"results": [{"id": 1, "title": "mug", "time": "now", "text": "the green mug"}]}));
    let search = paths.lock()[0].clone();
    assert!(search.contains("query=mug") && search.contains("project=eve") && search.contains("limit=2"), "{search}");
    // a limit int() cannot read is 5; one out of range is clamped
    for (limit, want) in [(json!("lots"), "limit=5"), (json!(null), "limit=5"), (json!(99), "limit=20"), (json!(0), "limit=1"), (json!(2.9), "limit=2")] {
        paths.lock().clear();
        recall(true, &obj(json!({"query": "mug", "limit": limit})), Some(&url)).await;
        assert!(paths.lock()[0].contains(want), "{limit} -> {:?}", paths.lock());
    }
}

#[tokio::test]
async fn test_a_remember_on_the_bus_is_dropped_while_memory_is_off() {
    let tmp = tempfile::tempdir().unwrap();
    let bus = MemoryBus::new();
    bus.subscribe("/eve/memory/remember", |_t: &str, msg: &Obj| remember(None, msg));
    let (cap, _guard) = capture();
    bus.publish("/eve/memory/remember", obj(json!({"text": "The green mug lives on the left shelf."})));
    let logs = cap.text();
    assert!(logs.contains("remember dropped") && !logs.contains("left shelf"), "{logs}");
    assert_eq!(std::fs::read_dir(tmp.path()).unwrap().count(), 0);
}

// ---- the daemon itself (needs the daemon unit's assemble) -----------------------------------------------------

/// The robot's link, connected, keeping what it was sent.
#[derive(Default)]
struct FakeLink {
    sent: Mutex<Vec<Value>>,
}

#[async_trait]
impl BoardLink for FakeLink {
    fn connected(&self) -> bool {
        true
    }
    async fn wait_connected(&self) {}
    async fn send(&self, obj: &Value, _codec: Option<&str>) -> bool {
        self.sent.lock().push(obj.clone());
        true
    }
    async fn run(&self) {}
    async fn stop(&self) {}
}

fn daemon(dir: &Path, bus_cfg: BusConfig) -> Arc<Daemon> {
    let mut parts = DaemonParts::for_test(Arc::new(FakeLink::default()), dir);
    parts.bus_cfg = bus_cfg;
    Daemon::assemble(parts)
}

#[tokio::test]
async fn test_the_daemon_wires_remember_and_recall_onto_its_bus() {
    let tmp = tempfile::tempdir().unwrap();
    let d = daemon(tmp.path(), BusConfig::default());
    let memory = memory(tmp.path());
    d.st.lock().memory = Some(memory.clone());
    d.start_memory_sinks().await;
    let out = d.bus.call_service("/eve/memory/recall", obj(json!({"query": "mug"}))).await.unwrap();
    assert_eq!(Value::Object(out), json!({"results": [], "error": "claude-mem is off (EVE_CLAUDE_MEM=1)"}));
    d.bus.publish("/eve/memory/remember", obj(json!({"text": "The green mug lives on the left shelf.", "title": "Mug"})));
    let said: Vec<_> = all_lines(&memory.transcripts).into_iter().filter(|ln| s(ln, "kind") != "close").collect();
    assert_eq!(said.len(), 1);
    assert_eq!(s(&said[0], "text"), "Mug: The green mug lives on the left shelf.");
    d.stop_memory_sinks().await;
    let st = d.st.lock();
    assert!(st.rosbridge.is_none() && st.claude_mem_sink.is_none() && st.claude_mem_mirror.is_none());
}

#[tokio::test]
async fn test_the_daemon_publishes_a_kept_thought_and_a_bare_lesson() {
    let tmp = tempfile::tempdir().unwrap();
    let d = daemon(tmp.path(), BusConfig::default());
    d.publish_observation(&record());
    let e = &d.bus.recent("/eve/memory/observation")[0].msg;
    assert_eq!(e["thought"], "the chair is pushed in");
    d.publish_lesson("", "error", "Try again.");
    let e = &d.bus.recent("/eve/memory/lesson")[0].msg;
    assert!(e["action"] == "notice" && e["stage"] == "error" && e["topic"] == "");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_shutdown_closes_the_open_voice_conversation_and_forgets_it() {
    let tmp = tempfile::tempdir().unwrap();
    let memory = memory(tmp.path());
    let voice = memory.transcripts.new_conv("voice");
    assert!(memory.transcripts.append("voice", &voice, "owner", "say", "one"));
    let d = daemon(tmp.path(), BusConfig::default());
    {
        let mut st = d.st.lock();
        st.memory = Some(memory.clone());
        st.voice_conv = voice.clone();
    }
    d.close_open_conversations().await;
    let closes = memory.transcripts.conv_lines(&voice).iter().filter(|ln| s(ln, "kind") == "close").count();
    assert_eq!(closes, 1);
    assert_eq!(d.st.lock().voice_conv, "");
    d.close_open_conversations().await; // a second pass closes nothing twice
    assert_eq!(memory.transcripts.conv_lines(&voice).iter().filter(|ln| s(ln, "kind") == "close").count(), 1);
}

#[tokio::test]
async fn test_shutdown_with_memory_off_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    std::fs::create_dir(&state).unwrap();
    let d = daemon(&state, BusConfig::default());
    d.st.lock().voice_conv = "v-1".into();
    d.close_open_conversations().await;
    assert!(!tmp.path().join("memory").exists());
    assert_eq!(d.st.lock().voice_conv, "v-1");
    assert!(d.start_dream().is_none()); // memory off: no dream
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_the_daemon_drains_its_background_work() {
    let tmp = tempfile::tempdir().unwrap();
    let d = daemon(tmp.path(), BusConfig::default());
    let job = d.spawn("quick", async { tokio::time::sleep(Duration::from_millis(20)).await });
    d.drain_background().await;
    assert!(job.done() && !job.cancelled());
    let (conv, today, _) = d.voice_memory(None).await;
    assert!(conv.is_empty() && today.is_empty());
}
