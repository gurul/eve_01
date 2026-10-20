//! Unit `inlet_turn`: telegram.py 4774-5852 and 7340-7593 (inlet/history, turn, parts, media); tests: test_telegram.py photos and files (781-1056), G11 (1518-1551), receipts and the record (4708-4816), G12 memory (4817-5135), app builds (5136-5316), every round (5953-6030); test_telegram_toolsets.py.
//!
//! The shared fakes are in `support` (architect-owned); this unit's own go here.

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_brain::prelude::{BrainMemory, Obj, OnPhoto, SceneWatch, Thinker};
use eve_brain::telegram::{self as telegram, Inbound, InletOptions, TelegramInlet};
use support::*;

// ---- this unit's fakes ------------------------------------------------------------------------------------------

/// test_telegram.py's MEMORY_TOOL_SCHEMAS.
fn memory_tool_schemas() -> Vec<Value> {
    vec![
        json!({"type": "function", "name": "memory_search", "strict": true, "description": "Search memory.",
               "parameters": {"type": "object", "additionalProperties": false, "required": ["query", "days_back"],
                              "properties": {"query": {"type": "string"}, "days_back": {"type": ["integer", "null"]}}}}),
        json!({"type": "function", "name": "memory_read", "strict": true, "description": "Read a record or a day.",
               "parameters": {"type": "object", "additionalProperties": false, "required": ["ref"],
                              "properties": {"ref": {"type": "string"}}}}),
        json!({"type": "function", "name": "forget_preview", "strict": true, "description": "Count what would go.",
               "parameters": {"type": "object", "additionalProperties": false, "required": ["query"],
                              "properties": {"query": {"type": "string"}}}}),
        json!({"type": "function", "name": "forget_apply", "strict": true, "description": "Forget it.",
               "parameters": {"type": "object", "additionalProperties": false, "required": ["token"],
                              "properties": {"token": {"type": "string"}}}}),
    ]
}

/// memory.Memory as the build contract names it, around a REAL transcripts.Transcripts in a tmp folder, so what
/// the inlet writes is read back from disk. The profile, tools, handle_tool and star are stubs that record what
/// they were asked.
struct FakeMemory {
    transcripts: Arc<eve_transcripts::Transcripts>,
    profile_text: String,
    stars: Mutex<Vec<String>>,
    calls: Mutex<Vec<(String, Value, String)>>,
}

impl FakeMemory {
    fn new(root: &std::path::Path, profile: &str) -> Arc<Self> {
        std::fs::create_dir_all(root).unwrap();
        let config = eve_transcripts::TranscriptConfig {
            enabled: true,
            root: root.join("transcripts"),
            home: Some(root.to_path_buf()),
        };
        let transcripts = Arc::new(eve_transcripts::Transcripts::new(config, None, None));
        Arc::new(Self {
            transcripts,
            profile_text: profile.to_string(),
            stars: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        })
    }

    /// Every transcript line written today, in order.
    fn said(&self) -> Vec<Map<String, Value>> {
        let day = self.transcripts.day_of(self.transcripts.now());
        self.transcripts.lines(&day).iter().map(|l| (**l).clone()).collect()
    }

    /// `_rows(memory, closes=...)`: (who, kind, text) of each line.
    fn rows(&self, closes: bool) -> Vec<(String, String, String)> {
        let s = |l: &Map<String, Value>, k: &str| l.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        self.said()
            .iter()
            .filter(|l| closes || s(l, "kind") != "close")
            .map(|l| (s(l, "who"), s(l, "kind"), s(l, "text")))
            .collect()
    }
}

impl BrainMemory for FakeMemory {
    fn transcripts(&self) -> Arc<eve_transcripts::Transcripts> {
        self.transcripts.clone()
    }
    fn profile(&self) -> String {
        self.profile_text.clone()
    }
    fn profile_for_voice(&self) -> String {
        self.profile_text.chars().take(3000).collect()
    }
    fn today(&self, max_chars: i64, options: &eve_transcripts::Today) -> String {
        self.transcripts.today_block_with(max_chars, options)
    }
    fn star(&self, text: &str) -> Option<String> {
        self.stars.lock().push(text.to_string());
        Some(format!("- {text} (2026-09-23)"))
    }
    fn tools(&self, _voice: bool) -> Vec<Value> {
        memory_tool_schemas()
    }
    fn handle_tool(&self, name: &str, args: &Value, _since: Option<eve_transcripts::Dt>, channel: &str) -> Value {
        self.calls.lock().push((name.to_string(), args.clone(), channel.to_string()));
        if name == "memory_search" {
            return self.transcripts.search(args.get("query").and_then(Value::as_str).unwrap_or(""));
        }
        json!({"ok": true})
    }
}

/// The robot's eyes: `look` answers `view`, every time.
struct FakeScene {
    view: Value,
}

#[async_trait]
impl eve_head::Scene for FakeScene {
    async fn look(&self, _fresh_secs: f64, _newer_than: Option<f64>) -> Value {
        self.view.clone()
    }
    async fn locate(&self, _target: &str, _newer_than: Option<f64>) -> Value {
        json!({"ok": false})
    }
}

#[async_trait]
impl SceneWatch for FakeScene {}

/// `say(text)`: a reply that only says it.
fn say(text: &str) -> Value {
    reply(text)
}

/// `_in(text, message_id)`.
fn inbound(text: &str, message_id: i64) -> Inbound {
    Inbound { chat_id: OWNER, user_id: OWNER, text: text.to_string(), message_id, ..Default::default() }
}

/// The developer notes of one request.
fn developer(body: &Value) -> Vec<String> {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i.get("role").and_then(Value::as_str) == Some("developer"))
        .map(|i| i["content"][0]["text"].as_str().unwrap_or("").to_string())
        .collect()
}

/// The tool names a request offers.
fn names(body: &Value) -> Vec<String> {
    body["tools"].as_array().unwrap().iter().map(|t| t.get("name").and_then(Value::as_str).unwrap_or("").to_string()).collect()
}

/// The JSON of the last function_call_output of a request.
fn last_output(body: &Value) -> Value {
    let item = body["input"].as_array().unwrap().last().unwrap();
    serde_json::from_str(item["output"].as_str().unwrap()).unwrap()
}

/// Let the loop, the spawned turns and their worker threads run.
async fn settle_real() {
    for _ in 0..20 {
        settle(50).await;
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
}

/// `run_rig(rig, during)`: the poll loop runs over the fake updates, then `during`, then it is stopped.
async fn run_rig<F, Fut>(rig: &Rig, during: Option<F>)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let inlet = rig.inlet.clone();
    let loop_task = tokio::spawn(async move { inlet.run().await });
    settle_real().await;
    if let Some(during) = during {
        during().await;
        settle_real().await;
    }
    assert!(!loop_task.is_finished(), "the poll loop died");
    loop_task.abort();
    let _ = loop_task.await;
    settle_real().await;
}

/// `run_rig(rig)` with nothing during.
async fn run_plain(rig: &Rig) {
    run_rig(rig, None::<fn() -> std::future::Ready<()>>).await;
}

/// A Rig whose spend ledger writes into the test's folder (never the owner's).
fn rig(api: Arc<FakeApi>, create: Arc<FakeCreate>, tweak: impl FnOnce(&mut InletOptions)) -> Rig {
    let spend = tmp();
    eve_spend::set_dir(Some(spend.keep()));
    Rig::new(api, create, config(), tweak)
}

fn requests(rig: &Rig) -> Vec<Value> {
    rig.create.requests.lock().clone()
}

fn sent(rig: &Rig) -> Vec<(i64, String)> {
    rig.api.log.lock().sent.clone()
}

/// What the tracing log said while `f` ran on this thread.
struct Logs(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Logs {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn capture_logs() -> (Arc<Mutex<Vec<u8>>>, tracing::subscriber::DefaultGuard) {
    let buf = Arc::new(Mutex::new(Vec::new()));
    let writer = buf.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .with_ansi(false)
        .without_time()
        .with_writer(move || Logs(writer.clone()))
        .finish();
    (buf, tracing::subscriber::set_default(subscriber))
}

fn log_text(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buf.lock()).into_owned()
}

// ---- G9: photos -------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_photo_is_sent_as_a_photo() {
    let dir = tmp();
    let picture = path(&dir, "desk.jpg");
    std::fs::write(&picture, b"\xff\xd8jpeg").unwrap();
    let notes = Arc::new(Mutex::new(Vec::<String>::new()));
    let (n, p) = (notes.clone(), picture.clone());
    let on_photo: OnPhoto = Arc::new(move |note: String| {
        n.lock().push(note);
        let p = p.clone();
        Box::pin(async move {
            json!({"ok": true, "path": p.display().to_string(), "caption": "A tidy desk with a mug.", "answer": "Kept it"})
                .as_object()
                .unwrap()
                .clone()
        })
    });
    let rig = rig(
        FakeApi::new(vec![vec![update(1, "send me a photo of my desk", OWNER, NOW, 1)]]),
        FakeCreate::new(vec![call("take_photo", json!({"note": "my desk"}), "c1"), say("Here's your desk!")]),
        |o| o.on_photo = Some(on_photo),
    );
    run_plain(&rig).await;
    assert_eq!(*notes.lock(), vec!["my desk".to_string()]);
    assert_eq!(rig.api.log.lock().photos, vec![(OWNER, picture.display().to_string(), "A tidy desk with a mug.".to_string())]);
    assert_eq!(sent(&rig), vec![(OWNER, "Here's your desk!".to_string())]);
}

#[tokio::test]
async fn test_no_camera_is_said_not_sent() {
    let no_camera: OnPhoto = Arc::new(|_note: String| {
        Box::pin(async { json!({"ok": false, "reason": "the robot is not connected, so there is no camera"}).as_object().unwrap().clone() })
    });
    for lent in [Some(no_camera), None] {
        let rig = rig(
            FakeApi::new(vec![vec![update(1, "photo please", OWNER, NOW, 1)]]),
            FakeCreate::new(vec![call("take_photo", json!({"note": ""}), "c1"), say("My camera's off right now.")]),
            |o| o.on_photo = lent,
        );
        run_plain(&rig).await;
        assert!(rig.api.log.lock().photos.is_empty());
        assert_eq!(sent(&rig), vec![(OWNER, "My camera's off right now.".to_string())]);
        assert_eq!(last_output(&requests(&rig)[1])["ok"], json!(false));
    }
}

// ---- G9b: the visual verification layer ------------------------------------------------------

#[tokio::test]
async fn test_a_task_that_was_asked_to_show_something_arrives_with_the_screen_it_left() {
    let dir = Arc::new(tmp());
    let shots = Arc::new(Mutex::new(Vec::<PathBuf>::new()));
    let (d, s) = (dir.clone(), shots.clone());
    let screen = Arc::new(move || {
        let path = path(&d, &format!("screen-{}.jpg", s.lock().len()));
        std::fs::write(&path, b"\xff\xd8shot").unwrap();
        s.lock().push(path.clone());
        Some(path)
    });
    let goal = "show me the top headline on Google News";
    let r = rig(
        FakeApi::new(vec![vec![update(1, goal, OWNER, NOW, 1)]]),
        FakeCreate::new(vec![call("start_task", json!({"goal": goal}), "c1")]),
        {
            let screen = screen.clone();
            move |o| o.screen = Some(screen)
        },
    );
    let agents = r.agents.clone();
    run_rig(&r, Some(|| async move { agents.last().release.set() })).await;
    assert_eq!(sent(&r), vec![(OWNER, telegram::ON_IT_LINE.to_string()), (OWNER, "Calculator is open.".to_string())]);
    let photos = r.api.log.lock().photos.clone();
    assert!(photos.len() == 1 && photos[0].0 == OWNER && photos[0].2.contains("task ended"));
    assert!(!shots.lock().is_empty() && !shots.lock()[0].exists()); // the temp file is gone once it is sent
    // a request that did not ask to see anything gets the words only (owner, 2026-09-21)
    let r = rig(
        FakeApi::new(vec![vec![update(1, "open the calculator", OWNER, NOW, 1)]]),
        FakeCreate::new(vec![call("start_task", json!({"goal": "open the calculator"}), "c1")]),
        move |o| o.screen = Some(screen),
    );
    let agents = r.agents.clone();
    run_rig(&r, Some(|| async move { agents.last().release.set() })).await;
    assert!(r.api.log.lock().photos.is_empty());
    assert_eq!(sent(&r).last().cloned(), Some((OWNER, "Calculator is open.".to_string())));
    for asks in ["give me a screenshot of the headline", "send me a picture of the screen", "what does my calendar look like"] {
        assert!(telegram::WANTS_SCREEN.is_match(asks), "{asks}");
    }
    for plain in ["open the calculator", "play some music", "pause"] {
        assert!(!telegram::WANTS_SCREEN.is_match(plain), "{plain}");
    }
}

#[tokio::test]
async fn test_the_owner_can_ask_for_the_screen_and_a_failed_capture_is_said() {
    let dir = tmp();
    let shot = path(&dir, "s.jpg");
    std::fs::write(&shot, b"\xff\xd8shot").unwrap();
    let s = shot.clone();
    let r = rig(
        FakeApi::new(vec![vec![update(1, "how does the calendar look right now?", OWNER, NOW, 1)]]),
        FakeCreate::new(vec![call("screenshot", json!({"caption": "Here's your screen"}), "c1"), say("Sent!")]),
        move |o| o.screen = Some(Arc::new(move || Some(s.clone()))),
    );
    run_plain(&r).await;
    assert_eq!(r.api.log.lock().photos, vec![(OWNER, shot.display().to_string(), "Here's your screen".to_string())]);
    assert_eq!(sent(&r), vec![(OWNER, "Sent!".to_string())]);
    let r = rig(
        FakeApi::new(vec![vec![update(1, "how does the calendar look right now?", OWNER, NOW, 1)]]),
        FakeCreate::new(vec![call("screenshot", json!({"caption": "x"}), "c1"), say("I couldn't grab the screen.")]),
        |o| o.screen = Some(Arc::new(|| None)),
    );
    run_plain(&r).await;
    assert!(r.api.log.lock().photos.is_empty());
    assert!(last_output(&requests(&r)[1])["reason"].as_str().unwrap().contains("Screen Recording"));
}

// ---- G11: memory ------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_quiet_chat_is_handed_to_memory_once() {
    // Rust: Rig cannot wrap _close_chat, so the close is read from what it does: the RAM history is cleared.
    let api = FakeApi::new(vec![vec![update(1, "remember the eyes look great", OWNER, NOW, 1)], vec![], vec![], vec![]]);
    let r = rig(api.clone(), FakeCreate::new(vec![say("They do!")]), |_| {});
    let inlet = r.inlet.clone();
    let loop_task = tokio::spawn(async move { inlet.run().await });
    for _ in 0..200 {
        settle_real().await;
        if api.log.lock().polls >= 2 && !r.inlet.turns().is_empty() {
            r.clock.set(601.0); // ten quiet minutes later
        }
        if r.clock.now() >= 601.0 && r.inlet.turns().is_empty() {
            break;
        }
    }
    loop_task.abort();
    let _ = loop_task.await;
    assert_eq!(sent(&r), vec![(OWNER, "They do!".to_string())]);
    assert!(r.inlet.turns().is_empty());
}

#[tokio::test]
async fn test_a_chat_still_open_at_shutdown_is_not_lost() {
    // Rust: the close is read from the transcript (its close marker), not from a wrapped _close_chat.
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "");
    let m = memory.clone();
    let r = rig(
        FakeApi::new(vec![vec![update(1, "bye for now", OWNER, NOW, 1)]]),
        FakeCreate::new(vec![say("See you!")]),
        move |o| o.memory = Some(m),
    );
    run_plain(&r).await;
    assert_eq!(
        memory.rows(true),
        vec![
            ("owner".into(), "say".into(), "bye for now".into()),
            ("eve".into(), "say".into(), "See you!".into()),
            ("system".into(), "close".into(), "".into()),
        ]
    );
}

#[tokio::test]
async fn test_history_is_bounded() {
    let r = rig(FakeApi::new(vec![]), FakeCreate::new(vec![]), |_| {});
    for i in 0..100 {
        r.inlet.note(if i % 2 == 0 { "user" } else { "eve" }, &format!("turn {i}"), "say", None, None, None, false, None);
    }
    let items = r.inlet.history();
    assert_eq!(items.len(), telegram::HISTORY_TURNS);
    assert_eq!(items.last().unwrap()["content"][0]["text"], json!("turn 99"));
}

// ---- review fixes: receipts, labels and the chat's record (2026-09-23) ----------------------------------

#[tokio::test]
async fn test_a_round_that_stars_a_fact_and_keeps_a_note_says_where_the_note_went() {
    let dir = tmp();
    let vault = Arc::new(eve_second_brain::VaultConfig { enabled: true, root: dir.path().join("vault") });
    let both = json!({"id": "resp", "output": [
        {"type": "function_call", "name": "capture_note", "call_id": "a",
         "arguments": serde_json::to_string(&json!({"text": "buy milk", "kind": "todo"})).unwrap()},
        {"type": "function_call", "name": "remember", "call_id": "b",
         "arguments": serde_json::to_string(&json!({"claim": "I like ramen"})).unwrap()}]});
    let memory = FakeMemory::new(&dir.path().join("m"), "");
    let api = FakeApi::new(vec![]);
    let r = rig(api.clone(), FakeCreate::new(vec![both]), move |o| {
        o.vault = Some(vault);
        o.memory = Some(memory);
    });
    r.inlet.turn(&inbound("todo: buy milk, and remember I like ramen", 4), None, &[], false, false).await;
    assert_eq!(api.log.lock().reactions, vec![(OWNER, 4, telegram::STAR_REACTION.to_string())]);
    let sent = sent(&r);
    assert!(sent.len() == 1 && sent[0].1.starts_with("Added to the todo list ("), "{sent:?}");
    assert_eq!(requests(&r).len(), 1);
}

#[test]
fn test_the_remember_guidance_says_the_reaction_is_the_confirmation() {
    let text = telegram::INSTRUCTIONS.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(text.contains("For remember, write nothing alongside the call"));
}

// ---- G12: memory — every word to the transcript, today in the prompt, one set of memory tools ------------

#[tokio::test]
async fn test_the_clock_is_in_the_developer_note_right_before_the_owners_message() {
    // Rust: system_context.context cannot be patched; the note is read as the one developer item instead.
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "# The owner\n- Likes tea");
    let r = rig(FakeApi::new(vec![]), FakeCreate::new(vec![say("Hi."), say("Tea.")]), move |o| o.memory = Some(memory));
    r.inlet.turn(&inbound("hello", 1), None, &[], false, false).await;
    r.inlet.turn(&inbound("what do I drink?", 2), None, &[], false, false).await;
    let bodies = requests(&r);
    assert_eq!(bodies.len(), 2);
    for body in &bodies {
        let roles: Vec<&str> = body["input"].as_array().unwrap().iter().map(|i| i["role"].as_str().unwrap_or("")).collect();
        assert_eq!(&roles[roles.len() - 2..], ["developer", "user"]);
        assert_eq!(roles.iter().filter(|r| **r == "developer").count(), 1);
        let note = &developer(body)[0];
        assert!(!body["instructions"].as_str().unwrap().contains(note.as_str()));
    }
    let notes: Vec<String> = bodies.iter().map(|b| developer(b)[0].clone()).collect();
    assert!(r.inlet.turns().iter().all(|(_, text)| !notes.contains(text))); // never kept in the history
}

#[tokio::test]
async fn test_the_developer_note_is_the_same_in_every_round_of_one_turn() {
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "");
    let scene = Arc::new(FakeScene { view: json!({"ok": true, "view": "a desk"}) });
    let r = rig(FakeApi::new(vec![]), FakeCreate::new(vec![call("look", json!({}), "c1"), say("A desk.")]), move |o| {
        o.memory = Some(memory);
        o.scene = Some(scene);
    });
    r.inlet.turn(&inbound("what do you see?", 1), None, &[], false, false).await;
    let bodies = requests(&r);
    let (a, b) = (&bodies[0], &bodies[1]);
    assert_eq!(developer(a), developer(b));
    assert_eq!(developer(a).len(), 1);
    assert_eq!(a["instructions"], b["instructions"]);
}

#[tokio::test]
async fn test_a_turn_is_written_to_the_transcript_as_it_happens() {
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "");
    let m = memory.clone();
    let r = rig(FakeApi::new(vec![vec![update(1, "hello there", OWNER, NOW, 1)]]), FakeCreate::new(vec![say("Hi!")]), move |o| {
        o.memory = Some(m)
    });
    run_plain(&r).await; // the loop ends: the chat is closed at shutdown
    let lines = memory.said();
    assert_eq!(
        memory.rows(true),
        vec![
            ("owner".into(), "say".into(), "hello there".into()),
            ("eve".into(), "say".into(), "Hi!".into()),
            ("system".into(), "close".into(), "".into()),
        ]
    );
    let convs: std::collections::HashSet<Value> = lines.iter().map(|l| l["conv"].clone()).collect();
    let chans: std::collections::HashSet<Value> = lines.iter().map(|l| l["ch"].clone()).collect();
    assert_eq!(convs.len(), 1);
    assert_eq!(chans, [json!("telegram")].into_iter().collect::<std::collections::HashSet<Value>>());
    // the control: with no memory lent nothing is minted or written
    let r = rig(FakeApi::new(vec![vec![update(1, "hello there", OWNER, NOW, 1)]]), FakeCreate::new(vec![say("Hi!")]), |_| {});
    run_plain(&r).await;
    assert!(r.inlet.conversation().is_none() && r.inlet.turn_kinds().is_empty());
}

#[tokio::test]
async fn test_code_words_are_commands_and_codex_traffic_is_relay_outside_the_history() {
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "");
    let m = memory.clone();
    let api = FakeApi::new(vec![
        vec![update(1, "claude off", OWNER, NOW, 1)],
        vec![update(2, "stealth", OWNER, NOW, 2)],
        vec![update(3, "codex: fix the tests", OWNER, NOW, 3)],
    ]);
    let r = rig(api, FakeCreate::new(vec![]), move |o| o.memory = Some(m));
    run_plain(&r).await;
    assert_eq!(
        memory.rows(false),
        vec![
            ("owner".into(), "command".into(), "claude off".into()),
            ("owner".into(), "command".into(), "stealth".into()),
            ("owner".into(), "relay".into(), "codex: fix the tests".into()),
        ]
    );
    assert!(r.inlet.turns().is_empty()); // neither code words nor Codex reach the history
}

#[tokio::test]
async fn test_an_image_turn_is_an_image_line_with_its_caption() {
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "");
    let m = memory.clone();
    let r = rig(FakeApi::new(vec![]), FakeCreate::new(vec![say("A cat.")]), move |o| o.memory = Some(m));
    let image = eve_telegram_images::ReceivedImage::new(vec![0xFF, 0xD8, 0xFF, 0xD9], "image/jpeg", ".jpg");
    r.inlet.turn(&inbound("what is this?", 1), Some(&image), &[], false, false).await;
    assert_eq!(
        memory.rows(false),
        vec![("owner".into(), "image".into(), "what is this?".into()), ("eve".into(), "say".into(), "A cat.".into())]
    );
    assert_eq!(r.inlet.turns()[0], ("user".to_string(), "what is this? [image attached]".to_string()));
}

#[tokio::test]
async fn test_an_idle_close_writes_the_close_marker_first_and_the_next_text_opens_a_new_conversation() {
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "");
    let m = memory.clone();
    let r = rig(FakeApi::new(vec![]), FakeCreate::new(vec![say("One."), say("Two.")]), move |o| o.memory = Some(m));
    r.inlet.turn(&inbound("the first text", 1), None, &[], false, false).await;
    let first = r.inlet.conversation();
    r.clock.set(599.0);
    r.inlet.close_quiet_chat();
    assert_eq!(r.inlet.conversation(), first); // not quiet long enough
    r.clock.set(601.0);
    r.inlet.close_quiet_chat();
    assert!(r.inlet.conversation().is_none() && r.inlet.turns().is_empty());
    let kinds: Vec<String> = memory.said().iter().map(|l| l["kind"].as_str().unwrap().to_string()).collect();
    assert_eq!(kinds, ["say", "say", "close"]);
    r.inlet.turn(&inbound("the second text", 2), None, &[], false, false).await;
    let second = r.inlet.conversation();
    let (first, second) = (first.unwrap(), second.unwrap());
    assert!(!first.is_empty() && !second.is_empty() && first != second);
    let convs: Vec<String> = memory.said().iter().map(|l| l["conv"].as_str().unwrap().to_string()).collect();
    assert_eq!(convs, [first.clone(), first.clone(), first, second.clone(), second]);
    // the RAM history is gone, but the words are not: the closed chat is in the next turn's today block
    let body = &requests(&r)[1];
    let roles: Vec<&str> = body["input"].as_array().unwrap().iter().map(|i| i["role"].as_str().unwrap()).collect();
    assert_eq!(roles, ["developer", "user"]);
    let instructions = body["instructions"].as_str().unwrap();
    assert!(instructions.contains("(texted) Owner: the first text") && instructions.contains("eve: One."));
}

#[tokio::test]
async fn test_a_voice_line_said_between_two_texts_is_in_the_second_turns_today_block() {
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "");
    let m = memory.clone();
    let r = rig(FakeApi::new(vec![]), FakeCreate::new(vec![say("Noted."), say("Thursday.")]), move |o| o.memory = Some(m));
    r.inlet.turn(&inbound("hi", 1), None, &[], false, false).await;
    let voice = memory.transcripts.new_conv("voice");
    assert!(memory.transcripts.append("voice", &voice, "owner", "say", "I moved the dentist to Thursday"));
    r.inlet.turn(&inbound("when is my dentist?", 2), None, &[], false, false).await;
    let bodies = requests(&r);
    let (first, second) = (bodies[0]["instructions"].as_str().unwrap(), bodies[1]["instructions"].as_str().unwrap());
    assert!(!first.contains("dentist"));
    assert!(second.contains("(spoken) Owner: I moved the dentist to Thursday"));
    assert!(second.find(telegram::TODAY_HEADER).unwrap() < second.find("dentist").unwrap());
}

/// The today block leaves out exactly the lines the history shows: the history's say lines plus the
/// owner's new message. A code word is in neither (the transcript's only), so it does not count. A fixed
/// exclude_tail=HISTORY_TURNS fails both cases: with no command it repeats one line, with two it loses one.
///
/// Rust: a code word is noted as `_handle` notes it (`self._note("user", inbound.text, "command")`, then its own
/// line, which is never noted); `_handle` is the crate's own, so the test makes the same note.
async fn no_line_of_the_history_is_repeated_in_the_today_block_and_none_is_lost(commands: usize) {
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "");
    let earlier = memory.transcripts.new_conv("telegram");
    memory.transcripts.append("telegram", &earlier, "owner", "say", "an earlier chat today");
    let n = telegram::HISTORY_TURNS / 2 + 3; // enough turns to push the first ones out of the history
    let m = memory.clone();
    let r = rig(FakeApi::new(vec![]), FakeCreate::new((0..n).map(|i| say(&format!("reply {i}"))).collect()), move |o| {
        o.memory = Some(m)
    });
    for i in 0..n {
        if i == n - 4 {
            for c in 0..commands {
                // command lines inside the history window
                let word = if c % 2 == 0 { "stealth" } else { "stealth off" };
                r.inlet.note("user", word, "command", None, None, None, false, None);
            }
        }
        r.inlet.turn(&inbound(&format!("text {i}"), i as i64 + 1), None, &[], false, false).await;
    }
    let last = requests(&r).last().cloned().unwrap();
    let history: Vec<String> = last["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| matches!(i["role"].as_str(), Some("user" | "assistant")))
        .map(|i| i["content"][0]["text"].as_str().unwrap().to_string())
        .collect();
    let instructions = last["instructions"].as_str().unwrap();
    let block = instructions.split_once(telegram::TODAY_HEADER).unwrap().1;
    let mut said: Vec<String> =
        memory.said().iter().filter(|l| l["kind"] == json!("say")).map(|l| l["text"].as_str().unwrap().to_string()).collect();
    said.pop(); // the last reply came after it
    for text in &said {
        let shown = history.iter().filter(|h| *h == text).count()
            + block.split('\n').filter(|row| row.ends_with(&format!(": {text}"))).count();
        assert_eq!(shown, 1, "{text}: {shown}"); // every say line exactly once: history or block
    }
    assert!(block.contains("an earlier chat today") && block.contains("Owner: text 0")); // the positive controls
    // text 0 fell out; code words never enter
    assert!(!history.contains(&"text 0".to_string()) && !history.iter().any(|h| h.contains("stealth")));
    if commands > 0 {
        // the control: the code words are in the transcript, as commands
        assert_eq!(memory.said().iter().filter(|l| l["kind"] == json!("command")).count(), commands);
    }
}

#[tokio::test]
async fn test_no_line_of_the_history_is_repeated_in_the_today_block_and_none_is_lost_0() {
    no_line_of_the_history_is_repeated_in_the_today_block_and_none_is_lost(0).await;
}

#[tokio::test]
async fn test_no_line_of_the_history_is_repeated_in_the_today_block_and_none_is_lost_2() {
    no_line_of_the_history_is_repeated_in_the_today_block_and_none_is_lost(2).await;
}

#[tokio::test]
async fn test_the_memory_tools_are_offered_whenever_memory_is_lent_even_with_no_profile() {
    let dir = tmp();
    let schema_names: Vec<String> = memory_tool_schemas().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    let own: Vec<String> = telegram::own_tools().iter().map(|t| t["name"].as_str().unwrap().to_string()).collect();
    let memory = FakeMemory::new(&dir.path().join("a"), "");
    let r = rig(FakeApi::new(vec![]), FakeCreate::new(vec![say("ok")]), move |o| o.memory = Some(memory));
    r.inlet.turn(&inbound("hi", 1), None, &[], false, false).await;
    let body = &requests(&r)[0];
    let offered = names(body);
    let mut expected = own.clone();
    expected.extend(schema_names.clone());
    assert_eq!(offered[..own.len() + schema_names.len()], expected[..]); // a stable order
    let instructions = body["instructions"].as_str().unwrap();
    assert!(instructions.contains(telegram::MEMORY_HINT) && !instructions.contains(telegram::PROFILE_HEADER));
    // with a profile: the page under its header, and the same tools
    let memory = FakeMemory::new(&dir.path().join("b"), "# The owner\n- Tea");
    let r = rig(FakeApi::new(vec![]), FakeCreate::new(vec![say("ok")]), move |o| o.memory = Some(memory));
    r.inlet.turn(&inbound("hi", 1), None, &[], false, false).await;
    let body = &requests(&r)[0];
    assert!(body["instructions"].as_str().unwrap().contains(&format!("{}\n# The owner\n- Tea", telegram::PROFILE_HEADER)));
    assert!(schema_names.iter().all(|n| names(body).contains(n)));
    // the control: no memory, no memory tools
    let r = rig(FakeApi::new(vec![]), FakeCreate::new(vec![say("ok")]), |_| {});
    r.inlet.turn(&inbound("hi", 1), None, &[], false, false).await;
    assert!(!names(&requests(&r)[0]).iter().any(|n| schema_names.contains(n)));
}

#[tokio::test(flavor = "current_thread")]
async fn test_a_memory_tool_runs_through_the_memory_with_dated_hits_and_logs_counts_only() {
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "");
    let earlier = memory.transcripts.new_conv("voice");
    memory.transcripts.append("voice", &earlier, "owner", "say", "the plumber comes on Friday");
    let m = memory.clone();
    let r = rig(
        FakeApi::new(vec![]),
        FakeCreate::new(vec![call("memory_search", json!({"query": "plumber friday", "days_back": null}), "c1"), say("On Friday.")]),
        move |o| o.memory = Some(m),
    );
    let (logs, _guard) = capture_logs();
    r.inlet.turn(&inbound("when does the plumber come?", 1), None, &[], false, false).await;
    assert_eq!(
        *memory.calls.lock(),
        vec![("memory_search".to_string(), json!({"query": "plumber friday", "days_back": null}), "telegram".to_string())]
    );
    let out = last_output(&requests(&r)[1]);
    let hit = out["hits"].as_array().unwrap().iter().find(|h| h["text"] == json!("the plumber comes on Friday")).cloned().unwrap();
    assert_eq!(hit["day"], json!(memory.transcripts.day_of(memory.transcripts.now())));
    assert_eq!(hit["ch"], json!("voice"));
    let text = log_text(&logs);
    assert!(regex::Regex::new(r"memory_search: 1 hit in \d+ ms").unwrap().is_match(&text), "{text}");
    assert!(!text.contains("plumber") && !text.contains("Friday"));
    assert!(!memory.said().iter().any(|l| l["kind"] == json!("tool"))); // memory results are not re-recorded
}

#[tokio::test(flavor = "current_thread")]
async fn test_the_turn_log_line_has_token_counts_and_never_the_words() {
    let dir = tmp();
    let mut first = call("look", json!({}), "c1");
    let mut second = say("A red mug.");
    first["usage"] = json!({"input_tokens": 1000, "output_tokens": 20, "input_tokens_details": {"cached_tokens": 900}});
    second["usage"] = json!({"input_tokens": 1100, "output_tokens": 10, "input_tokens_details": {"cached_tokens": 1024}});
    let memory = FakeMemory::new(dir.path(), "");
    let m = memory.clone();
    let scene = Arc::new(FakeScene { view: json!({"ok": true, "view": "a red mug ".repeat(400)}) });
    let r = rig(FakeApi::new(vec![]), FakeCreate::new(vec![first, second]), move |o| {
        o.memory = Some(m);
        o.scene = Some(scene);
    });
    let (logs, _guard) = capture_logs();
    r.inlet.turn(&inbound("secret words about my mug", 1), None, &[], false, false).await;
    let text = log_text(&logs);
    let line = text.lines().find(|l| l.contains("turn answered")).unwrap().to_string();
    let instr = requests(&r)[0]["instructions"].as_str().unwrap().chars().count();
    assert!(line.ends_with(&format!("(2 model calls; in=2100 cached=1924 out=30 instr={instr} chars)")), "{line}");
    assert!(!text.contains("secret words") && !text.contains("red mug"));
    // what eve saw is in the transcript, as a tool line, capped
    let tool: Vec<Map<String, Value>> = memory.said().into_iter().filter(|l| l["kind"] == json!("tool")).collect();
    assert_eq!(tool.iter().map(|l| l["tool"].clone()).collect::<Vec<_>>(), vec![json!("look")]);
    let words = tool[0]["text"].as_str().unwrap();
    assert!(words.contains("a red mug") && words.chars().count() <= telegram::TOOL_LINE_CHARS);
}

#[tokio::test]
async fn test_think_hard_is_told_the_profile_and_today() {
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "# The owner\n- Runs on Tuesdays");
    let seen = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let s = seen.clone();
    let thinker: Thinker = Arc::new(move |question: String, context: String| {
        s.lock().push((question, context));
        Box::pin(async { Ok(json!({"ok": true, "answer": "Rest tomorrow."}).as_object().unwrap().clone()) })
    });
    let m = memory.clone();
    let r = rig(
        FakeApi::new(vec![]),
        FakeCreate::new(vec![call("think_hard", json!({"question": "run tomorrow?"}), "c1"), say("Rest.")]),
        move |o| {
            o.memory = Some(m);
            o.thinker = Some(thinker);
        },
    );
    r.inlet.turn(&inbound("I ran today. Should I run tomorrow?", 1), None, &[], false, false).await;
    let seen = seen.lock().clone();
    assert_eq!(seen.len(), 1);
    let (question, context) = &seen[0];
    assert_eq!(question, "run tomorrow?");
    assert!(context.starts_with("# The owner\n- Runs on Tuesdays\n\n"));
    assert!(context.contains("(texted) Owner: I ran today. Should I run tomorrow?"));
    assert!(context.chars().count() <= telegram::THINK_CONTEXT_CHARS);
    let tools: Vec<Value> = memory.said().into_iter().filter(|l| l["kind"] == json!("tool")).map(|l| l["tool"].clone()).collect();
    assert_eq!(tools, vec![json!("think_hard")]);
    // the control: no memory, and the thinker is asked with no context (Rust: "" is "no context")
    let asked = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let a = asked.clone();
    let old: Thinker = Arc::new(move |question: String, context: String| {
        a.lock().push((question, context));
        Box::pin(async { Ok(json!({"ok": true, "answer": "Rest."}).as_object().unwrap().clone()) })
    });
    let r = rig(
        FakeApi::new(vec![]),
        FakeCreate::new(vec![call("think_hard", json!({"question": "run tomorrow?"}), "c1"), say("Rest.")]),
        move |o| o.thinker = Some(old),
    );
    r.inlet.turn(&inbound("Should I run tomorrow?", 1), None, &[], false, false).await;
    assert_eq!(*asked.lock(), vec![("run tomorrow?".to_string(), String::new())]);
}

// ---- apps ----------------------------------------------------------------------------------------------------------

#[test]
fn test_the_eval_rules_stay_in_the_text_brain_instructions() {
    // From the 2026-09-24 Ori eval: they took gpt-6-luna from 11/16 to 14/16 and closed its API-key safety miss.
    let text = telegram::INSTRUCTIONS.split_whitespace().collect::<Vec<_>>().join(" ");
    for rule in [
        "Keep API keys out of this chat",
        "through start_task with the owner's words",
        "check the calendar or ask whether the owner means a flight",
        "ask one useful next question",
        "own the miss plainly and offer to remember the date",
    ] {
        assert!(text.contains(rule), "{rule}");
    }
}

#[tokio::test]
async fn test_a_text_turn_cut_off_by_a_restart_is_answered_with_one_line() {
    // Live, 2026-09-24 08:47:54: the owner's text got "typing…", the daemon restarted at 08:48:00, and no reply
    // ever came. The update was already acknowledged, so Telegram will not send it again: eve must say so.
    let api = FakeApi::new(vec![vec![update(7, "what should I cook tonight?", OWNER, NOW, 7)]]);
    let create = FakeCreate::new(vec![]);
    create.push(Scripted::Later(Box::new(|| Box::pin(futures::future::pending()))));
    let r = rig(api.clone(), create, |_| {});
    run_plain(&r).await; // the loop is cancelled mid-turn: the daemon stops
    assert_eq!(api.log.lock().replies, vec![(telegram::TURN_RESTART_LINE.to_string(), 7)]);
    assert_eq!(sent(&r), vec![(OWNER, telegram::TURN_RESTART_LINE.to_string())]);
    // the control: a turn that finished before the restart gets its answer and nothing else
    let api = FakeApi::new(vec![vec![update(8, "hi", OWNER, NOW, 8)]]);
    let r = rig(api.clone(), FakeCreate::new(vec![say("Hello!")]), |_| {});
    run_plain(&r).await;
    assert_eq!(sent(&r), vec![(OWNER, "Hello!".to_string())]);
    assert!(api.log.lock().replies.is_empty());
}

#[tokio::test]
async fn test_empty_citation_links_never_reach_the_phone() {
    // Live, 2026-09-24 23:35:17: a reply carried "([]())", an empty markdown link left by the hosted web
    // search's citations. It is taken out before the reply is kept or sent; real links and code are not.
    let reply = "Ramen Taro opens at 11 ([]()). The menu is online ([Taro](https://example.com/menu)) \
                 and so is a map []() [here]().\nIn code, `x = f([]())` stays.";
    let api = FakeApi::new(vec![vec![update(1, "when does the ramen place open?", OWNER, NOW, 1)]]);
    let r = rig(api, FakeCreate::new(vec![say(reply)]), |_| {});
    run_plain(&r).await;
    let sent_text = sent(&r)[0].1.clone();
    assert_eq!(
        sent_text,
        "Ramen Taro opens at 11. The menu is online ([Taro](https://example.com/menu)) and so is a map here.\nIn code, `x = f([]())` stays."
    );
    for (text, clean) in [("Done ([](https://a.example), []()).", "Done."), ("[]()", ""), ("plain", "plain")] {
        assert_eq!(telegram::strip_empty_links(text), clean, "{text}");
    }
}

// ---- a turn that uses every round (2026-09-30: the office-hours turns) ----------------------------------------

#[tokio::test(flavor = "current_thread")]
async fn test_a_turn_that_uses_every_round_says_what_it_found_instead_of_giving_up() {
    // Rust: web_search cannot be patched here, so each round is a look (a read, as web_search is) at a fake scene.
    let rounds: Vec<Value> = (0..telegram::MAX_TOOL_ROUNDS).map(|i| call("look", json!({}), &format!("c{i}"))).collect();
    let summary = say("I found the office hours: Mon 2:30 PM, Allen breakout. Adding them is left; say keep going.");
    let mut script = rounds;
    script.push(summary);
    let api = FakeApi::new(vec![vec![update(1, "find the office hours for my classes and add them to my calendar", OWNER, NOW, 1)]]);
    let scene = Arc::new(FakeScene { view: json!({"ok": true, "view": "Mon 2:30 PM"}) });
    let r = rig(api, FakeCreate::new(script), move |o| o.scene = Some(scene));
    let (logs, _guard) = capture_logs();
    run_plain(&r).await;
    assert_eq!(
        sent(&r).last().cloned(),
        Some((OWNER, "I found the office hours: Mon 2:30 PM, Allen breakout. Adding them is left; say keep going.".to_string()))
    );
    let reqs = requests(&r);
    assert_eq!(reqs.len(), telegram::MAX_TOOL_ROUNDS + 1);
    assert_eq!(reqs.len(), 11);
    assert!(reqs[..reqs.len() - 1].iter().all(|q| q["tool_choice"] == json!("auto")));
    let last = reqs.last().unwrap();
    assert_eq!(last["tool_choice"], json!("none"));
    assert_eq!(last["tools"], reqs[0]["tools"]); // same prefix, nothing callable
    assert!(last["input"][0]["content"][0]["text"].as_str().unwrap().contains(telegram::OUT_OF_ROUNDS_NOTE));
    assert!(!reqs[0]["input"][0]["content"][0]["text"].as_str().unwrap().contains(telegram::OUT_OF_ROUNDS_NOTE));
    assert!(sent(&r).iter().all(|(_, t)| !t.contains(telegram::TANGLED_LINE)));
    // every round's tools are in the log, by name only
    let text = log_text(&logs);
    let rounds_logged: Vec<&str> = text.lines().filter(|l| l.contains(": round ")).collect();
    assert_eq!(rounds_logged.len(), telegram::MAX_TOOL_ROUNDS);
    assert!(rounds_logged[0].ends_with("round 1: look"), "{}", rounds_logged[0]);
}

#[tokio::test]
async fn test_when_the_summary_itself_fails_the_old_line_still_goes() {
    let create = FakeCreate::new((0..telegram::MAX_TOOL_ROUNDS).map(|i| call("look", json!({}), &format!("c{i}"))).collect());
    create.push(Scripted::Fail("model down".into()));
    let scene = Arc::new(FakeScene { view: json!({"ok": true, "view": "x"}) });
    let r = rig(FakeApi::new(vec![vec![update(1, "do a long thing", OWNER, NOW, 1)]]), create, move |o| o.scene = Some(scene));
    run_plain(&r).await;
    assert_eq!(sent(&r).last().cloned(), Some((OWNER, telegram::TANGLED_LINE.to_string())));
}

#[tokio::test]
async fn test_a_turn_that_finishes_in_time_makes_no_extra_call() {
    let r = rig(
        FakeApi::new(vec![vec![update(1, "one photo please", OWNER, NOW, 1)]]),
        FakeCreate::new(vec![call("take_photo", json!({}), "c1"), say("No camera here.")]),
        |_| {},
    );
    run_plain(&r).await;
    assert_eq!(requests(&r).len(), 2);
    assert_eq!(sent(&r).last().cloned(), Some((OWNER, "No camera here.".to_string())));
}

#[test]
fn test_call_names_log_slugs_never_arguments() {
    let calls: Vec<Obj> = vec![
        json!({"name": "COMPOSIO_MULTI_EXECUTE_TOOL", "args": {"tools": [
            {"tool_slug": "GMAIL_FETCH_EMAILS", "arguments": {"query": "from:sam secret words"}},
            {"tool_slug": "GOOGLECALENDAR_CREATE_EVENT", "arguments": {"summary": "private"}}]}}),
        json!({"name": "web_search", "args": {"query": "private query"}}),
    ]
    .into_iter()
    .map(|v| v.as_object().unwrap().clone())
    .collect();
    let line = telegram::call_names(&calls);
    assert_eq!(line, "COMPOSIO_MULTI_EXECUTE_TOOL[GMAIL_FETCH_EMAILS,GOOGLECALENDAR_CREATE_EVENT], web_search");
    assert!(!line.contains("secret") && !line.contains("private"));
}

// ---- the turn's own pieces, read directly ----------------------------------------------------------------------

#[test]
fn test_count_usage_adds_ints_only_and_a_reply_without_usage_adds_nothing() {
    let mut usage: std::collections::HashMap<String, i64> =
        ["in", "cached", "out", "instr"].iter().map(|k| (k.to_string(), 0)).collect();
    TelegramInlet::count_usage(&json!({"usage": {"input_tokens": 10, "output_tokens": 3, "input_tokens_details": {"cached_tokens": 4}}}), &mut usage);
    TelegramInlet::count_usage(&json!({"usage": {"input_tokens": true, "output_tokens": 2.5, "input_tokens_details": "x"}}), &mut usage);
    TelegramInlet::count_usage(&json!({"output": []}), &mut usage);
    TelegramInlet::count_usage(&json!([1, 2]), &mut usage);
    assert_eq!((usage["in"], usage["cached"], usage["out"], usage["instr"]), (10, 4, 3, 0));
}

#[test]
fn test_receipt_of_reads_the_telegram_table_outside_an_app_turn() {
    let call = |name: &str, args: Value| json!({"name": name, "args": args}).as_object().unwrap().clone();
    assert_eq!(TelegramInlet::receipt_of(&call("capture_note", json!({})), None), Some(telegram::VAULT_REACTION.to_string()));
    assert_eq!(TelegramInlet::receipt_of(&call("remember", json!({})), None), Some(telegram::STAR_REACTION.to_string()));
    // take_notes is a receipt only in the app (APP_RECEIPT_TOOLS)
    assert_eq!(TelegramInlet::receipt_of(&call("take_notes", json!({"action": "start"})), None), None);
    assert_eq!(TelegramInlet::receipt_of(&call("web_search", json!({})), None), None);
}

#[tokio::test]
async fn test_receipt_of_in_an_app_turn_counts_take_notes_only_when_it_starts() {
    let call = |name: &str, args: Value| json!({"name": name, "args": args}).as_object().unwrap().clone();
    let scope = eve_brain::prelude::TurnCtx { app: Some("t1".into()), ..Default::default() };
    eve_brain::prelude::in_ctx(scope, async {
        assert_eq!(TelegramInlet::receipt_of(&call("take_notes", json!({"action": "start"})), None), Some(telegram::VAULT_REACTION.to_string()));
        assert_eq!(TelegramInlet::receipt_of(&call("take_notes", json!({"action": "start"})), Some(&json!({"already": true}))), None);
        assert_eq!(TelegramInlet::receipt_of(&call("take_notes", json!({"action": "stop"})), None), None);
        assert_eq!(TelegramInlet::receipt_of(&call("remember", json!({})), None), Some(telegram::STAR_REACTION.to_string()));
    })
    .await;
}

#[test]
fn test_shown_counts_say_and_image_lines_only() {
    let kinds: Vec<String> = ["say", "", "image", "command", "say", "relay"].iter().map(|k| k.to_string()).collect();
    assert_eq!(TelegramInlet::shown(&kinds), 3);
    assert_eq!(TelegramInlet::shown(&[]), 0);
}

// ---- test_telegram_toolsets.py ----------------------------------------------------------------------------------

#[tokio::test]
async fn test_off_never_asks_the_router_and_offers_every_toolset_as_before() {
    // Rust: the part of the Python test with no Share or Composio fake (those live in other units' tests): off
    // never asks the router, and the parts carry no lent block.
    let never: eve_toolset_router::Router = Arc::new(|_: &str, _: &str| panic!("the router was asked with the switch off"));
    let r = rig(FakeApi::new(vec![vec![update(1, "hi", OWNER, NOW, 1)]]), FakeCreate::new(vec![say("Hello!")]), move |o| {
        o.toolset_mode = Some("off".into());
        o.toolset_route = eve_brain::prelude::Lent::Given(Some(never));
    });
    run_plain(&r).await;
    let first = &requests(&r)[0];
    assert!(!names(first).contains(&eve_apps_mcp::OPEN_TOOL.to_string())); // no direct app, so nothing to open
    assert_eq!(sent(&r), vec![(OWNER, "Hello!".to_string())]);
    let (parts, _allowed, _tools) = r.inlet.turn_parts(0, false).await;
    assert!(!parts.contains_key("lent_block"));
}

#[tokio::test]
async fn test_open_app_with_no_toolset_here_names_the_ones_that_are_and_not_the_asked_name() {
    // Rust: with no Share lent (its fake is the share unit's), the toolsets here are none; the reason still never
    // repeats the asked name.
    let r = rig(FakeApi::new(vec![]), FakeCreate::new(vec![]), |o| o.toolset_mode = Some("auto".into()));
    let out = r.inlet.open_toolsets(json!({"apps": ["zzq"]}).as_object().unwrap());
    assert_eq!(out["ok"], json!(false));
    assert!(!out["reason"].as_str().unwrap().contains("zzq"));
    assert!(out["reason"].as_str().unwrap().starts_with("that is not one of the toolsets here; the ones here: "));
}

#[tokio::test]
async fn test_the_call_warm_up_opens_no_toolset_left_over_from_the_last_turn() {
    // Rust: no Share or direct-app fake here; the warm-up's request is the one model call, cut to 16 tokens.
    let r = rig(FakeApi::new(vec![]), FakeCreate::new(vec![say("")]), |o| o.toolset_mode = Some("auto".into()));
    r.inlet.warm().await;
    let bodies = requests(&r);
    assert_eq!(bodies.len(), 1);
    let body = &bodies[0];
    assert_eq!(body["max_output_tokens"], json!(16));
    assert_eq!(body["reasoning"], json!({"effort": "low"}));
    let input = body["input"].as_array().unwrap();
    assert_eq!(input.last().unwrap()["content"][0]["text"], json!("(the call connected)"));
}

#[tokio::test]
async fn test_one_log_line_per_turn_names_the_toolsets_how_and_jevs_ms_never_the_words() {
    let hand_off = "get my resume over to ana before her interview";
    let asked = Arc::new(Mutex::new(Vec::<(String, String)>::new()));
    let a = asked.clone();
    let router: eve_toolset_router::Router = Arc::new(move |text: &str, previous: &str| {
        a.lock().push((text.to_string(), previous.to_string()));
        Ok(eve_toolset_router::Route { toolsets: ["calendar".to_string(), eve_toolset_router::SHARE.to_string()].into_iter().collect(), ms: 180.0, error: String::new() })
    });
    let (logs, _guard) = capture_logs();
    let said = format!("whens my thing w the advisor, and {hand_off}");
    let r = rig(FakeApi::new(vec![vec![update(1, &said, OWNER, NOW, 1)]]), FakeCreate::new(vec![say("ok")]), move |o| {
        o.toolset_mode = Some("auto".into());
        o.toolset_route = eve_brain::prelude::Lent::Given(Some(router));
    });
    run_plain(&r).await;
    assert_eq!(*asked.lock(), vec![(said.clone(), String::new())]);
    let text = log_text(&logs);
    assert!(text.lines().filter(|l| l.contains("telegram: toolsets:")).count() == 1, "{text}");
    assert!(!text.contains("advisor") && !text.contains("resume"));
}
