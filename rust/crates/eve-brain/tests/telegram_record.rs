//! test_telegram.py's reply links, the clock and the cache, the chat's record, app pictures, restarts and the call's
//! sentence stream (4086-4110, 4835-4900, 4953-4965, 5138-5320, 5504-5515), ported with their names and assertions.
//!
//! `monkeypatch.setattr(system_context, "context", ...)` has no counterpart: the clock note is read from the request
//! itself (it is `system_context.context()` taken at the call, so it is compared with that taken just before and
//! just after), or handed to `request` as the turn's note, which is what the patched clock produced.

mod more;
mod support;

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_brain::prelude::{BrainMemory, Http, HttpError, HttpRequest, HttpResponse, HttpTransport, Sessions};
use eve_brain::telegram::{self as telegram, BotApi, Inbound, TelegramApi, TelegramConfig};
use more::*;
use support::*;

const REPO: &str = "/Users/g/repo";

// ---- an httpx.MockTransport ----------------------------------------------------------------------------------

struct Recorder {
    requests: Mutex<Vec<HttpRequest>>,
    result: Value,
}

#[async_trait]
impl HttpTransport for Recorder {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.requests.lock().push(request);
        Ok(HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: serde_json::to_vec(&json!({"ok": true, "result": self.result})).unwrap(),
        })
    }
}

fn recorder(result: Value) -> Arc<Recorder> {
    Arc::new(Recorder { requests: Mutex::new(Vec::new()), result })
}

fn bot_over(r: &Arc<Recorder>) -> BotApi {
    let http: Http = r.clone();
    BotApi::with_http("123456:AAsecretTOKENvalue", http)
}

fn method_and_body(r: &HttpRequest) -> (String, Value) {
    (r.url.rsplit('/').next().unwrap_or("").to_string(), serde_json::from_slice(&r.body).unwrap_or(Value::Null))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

#[tokio::test]
async fn test_the_bot_api_sends_reply_links_reply_boxes_and_keeps_buttons_on_an_edit() {
    let r = recorder(json!({"message_id": 9}));
    let api = bot_over(&r);
    api.send_message(OWNER, "the result", None, None, &[], 31, None).await.unwrap();
    api.send_message(OWNER, "which one?", None, None, &[], 0, Some("Your answer")).await.unwrap();
    let stop = vec![vec![("Stop".to_string(), "k.1".to_string(), "danger".to_string())]];
    api.edit_message(OWNER, 9, "step", None, None, Some(&stop)).await.unwrap();
    api.edit_message(OWNER, 9, "done", None, None, None).await.unwrap();
    api.close().await;
    let seen: Vec<(String, Value)> = r.requests.lock().iter().map(method_and_body).collect();
    assert_eq!(seen[0].1["reply_parameters"], json!({"message_id": 31, "allow_sending_without_reply": true}));
    assert_eq!(seen[1].1["reply_markup"], json!({"force_reply": true, "input_field_placeholder": "Your answer"}));
    assert!(seen[2].0 == "editMessageText" && seen[2].1["reply_markup"] == json!({"inline_keyboard": [[{"text": "Stop", "callback_data": "k.1", "style": "danger"}]]}));
    assert_eq!(seen[3].1["reply_markup"], json!({"inline_keyboard": []}));
}

// ---- G12: the clock and the prompt cache -----------------------------------------------------------------------

fn developer(body: &Obj) -> Vec<String> {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i.get("role") == Some(&json!("developer")))
        .map(|i| i["content"][0]["text"].as_str().unwrap().to_string())
        .collect()
}

/// `telegram.request(CFG, items, **kw)` with the keyword arguments these tests pass.
#[allow(clippy::too_many_arguments)]
fn request(items: &[Obj], profile: &str, app_tools: &[Obj], vault: bool, today: &str, memory_tools: &[Obj], context: Option<&str>) -> Obj {
    let environ = conftest_env();
    telegram::request_in(Some(&environ), &config(), items, "", profile, app_tools, vault, today, memory_tools, context, &[], "", &[], "", "", "", &[])
}

#[test]
fn test_with_no_memory_the_instructions_are_the_old_ones_but_for_the_clock() {
    // Pinned: with no Memory lent the instructions are byte for byte what they were before transcripts, with the one
    // planned change: the clock (and the brief, "" with memory off) moved out of them, into the developer note right
    // before the owner's message. No memory tool is offered.
    let app = obj(json!({"type": "function", "name": "COMPOSIO_MULTI_EXECUTE_TOOL", "parameters": {}})); // its block rides Composio's tools
    for vault in [false, true] {
        for apps in [vec![], vec![app.clone()]] {
            let mut legacy = format!("{}{}", telegram::INSTRUCTIONS, eve_brain::voice_agent::memory_block(""));
            if vault {
                legacy.push_str("\n\n");
                legacy.push_str(eve_second_brain::INSTRUCTIONS_BLOCK);
            }
            if !apps.is_empty() {
                // no direct app up: no sentence about them
                legacy.push_str(telegram::APPS_BLOCK);
                legacy.push_str(telegram::APPS_BLOCK_REST);
            }
            let before = eve_system_context::context(None, None).trim().to_string();
            let body = request(&[telegram::message_item("user", "hi")], "", &apps, vault, "", &[], None);
            let after = eve_system_context::context(None, None).trim().to_string();
            let instructions = body["instructions"].as_str().unwrap();
            assert_eq!(instructions, legacy, "vault={vault} apps={}", apps.len());
            assert!(!instructions.contains(&before) && !instructions.contains(&after));
            let note = developer(&body)[0].clone();
            assert!(note == before || note == after, "{note:?}");
            assert_eq!(
                body["input"],
                json!([telegram::message_item("developer", &note), telegram::message_item("user", "hi")])
            );
            let names: Vec<String> = body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t.get("name").or_else(|| t.get("type")).and_then(Value::as_str).unwrap_or("").to_string())
                .collect();
            assert!(!names.iter().any(|n| n.starts_with("memory_") || n.starts_with("forget_")));
        }
    }
}

#[test]
fn test_two_requests_five_seconds_apart_have_identical_instructions() {
    // The prompt-cache regression gate. The clock used to sit right after INSTRUCTIONS, inside the instructions, so
    // two requests built 5 s apart differed from that byte on and nothing after it (the profile, the vault, the apps)
    // could be served from the cache: this test fails on that ordering. The instructions now hold only what is stable
    // within a day; the clock is in the developer note.
    let clock = |t: &str| telegram::turn_context("", Some(&format!("\n\nSystem context:\nLocal clock: {t}.")));
    let (profile, today) = ("# The owner\n- Likes tea", "09:00 (spoken) Owner: good morning");
    let items = [telegram::message_item("user", "hi")];
    let tools: Vec<Obj> = memory_tool_schemas().into_iter().map(|t| t.as_object().cloned().unwrap()).collect();
    let a = request(&items, profile, &[], true, today, &tools, Some(&clock("10:00:00")));
    let b = request(&items, profile, &[], true, today, &tools, Some(&clock("10:00:05")));
    assert!(a["instructions"] == b["instructions"] && a["tools"] == b["tools"]);
    assert_ne!(developer(&a), developer(&b)); // the control: the clock did move
    assert!(a["prompt_cache_key"] == b["prompt_cache_key"] && a["prompt_cache_key"] == json!(telegram::PROMPT_CACHE_KEY));
    assert_eq!(telegram::PROMPT_CACHE_KEY, "eve-telegram");
    // most stable first: instructions, profile, vault, then today last (it only grows)
    let ins = a["instructions"].as_str().unwrap();
    assert!(ins.starts_with(telegram::INSTRUCTIONS) && ins.ends_with(&format!("{}{today}", telegram::TODAY_HEADER)));
    let at = |s: &str| ins.find(s).unwrap();
    assert!(at(profile) < at(eve_second_brain::INSTRUCTIONS_BLOCK) && at(eve_second_brain::INSTRUCTIONS_BLOCK) < at(today));
}

// ---- the chat's record ------------------------------------------------------------------------------------------

fn rows(memory: &FakeMemory) -> Vec<(String, String, String)> {
    memory
        .said()
        .iter()
        .filter(|l| l.get("kind").and_then(Value::as_str) != Some("close"))
        .map(|l| {
            let s = |k: &str| l.get(k).and_then(Value::as_str).unwrap_or("").to_string();
            (s("who"), s("kind"), s("text"))
        })
        .collect()
}

fn row(who: &str, kind: &str, text: &str) -> (String, String, String) {
    (who.to_string(), kind.to_string(), text.to_string())
}

#[tokio::test]
async fn test_what_claude_says_in_the_relay_is_in_the_transcript() {
    let _iso = isolated();
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "");
    let m = memory.clone();
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), move |o| o.memory = Some(m as Arc<dyn BrainMemory>));
    rig.inlet.tt_set_claude(true);
    rig.inlet.tt_set_chat_id(Some(OWNER));
    rig.inlet.relay_text("I fixed the failing test.", "/tmp/repo").await;
    settle(200).await;
    assert_eq!(rows(&memory), vec![row("claude", "relay", "I fixed the failing test.")]);
    assert!(rig.inlet.turns().is_empty());
}

#[tokio::test]
async fn test_relay_lines_and_code_words_never_reach_the_text_brains_history() {
    // Live, 2026-09-24 08:44: the brain's history was eight lines the owner had typed to Claude Code ("The expense
    // spllitter doesn't work", "Ultrathink", "Claude off"...), and think_hard built "the plan" from them. Relay and
    // command lines are the transcript's only; a genuine owner/eve exchange stays.
    let _iso = isolated();
    let dir = tmp();
    let memory = FakeMemory::new(dir.path(), "");
    let typed = Arc::new(Mutex::new(Vec::new()));
    let t = typed.clone();
    let m = memory.clone();
    let create = FakeCreate::new(vec![reply("Pasta tonight."), reply("Here is what I know.")]);
    let rig = Rig::new(FakeApi::new(vec![]), create, TelegramConfig { ask_permissions: true, ..config() }, move |o| {
        o.terminal = Some(Arc::new(move |_cwd: String, text: String| {
            t.lock().push(text);
            async { Ok(String::new()) }.boxed()
        }));
        o.claude_sessions = Some(Arc::new(|| Ok(Sessions::Now(vec![REPO.to_string()]))));
        o.memory = Some(m as Arc<dyn BrainMemory>);
    });
    rig.inlet.tt_relay_to(REPO);
    rig.inlet.tt_set_chat_id(Some(OWNER));
    let dispatch = |text: &str, n: i64| {
        rig.inlet.say_relay_probe().dispatch(&Value::Object(update(n, text, OWNER, NOW, n)));
    };
    rig.inlet.tt_turn(&Inbound { message_id: 1, ..Inbound::new(OWNER, OWNER, "what's for dinner?") }).await;
    rig.inlet.tt_set_claude(true);
    for (text, n) in [("The expense spllitter doesn't work", 2), ("claude: Ultrathink", 3), ("claude off", 4), ("stealth", 5)] {
        dispatch(text, n);
        rig.inlet.say_relay_probe().jobs().await;
    }
    rig.inlet.tt_turn(&Inbound { message_id: 6, ..Inbound::new(OWNER, OWNER, "so what's the plan?") }).await;
    rig.inlet.say_relay_probe().shutdown().await;
    assert_eq!(*typed.lock(), vec!["The expense spllitter doesn't work", "Ultrathink"]);
    let last = rig.create.requests.lock().last().cloned().unwrap();
    let shown: Vec<(String, String)> = last["input"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i.get("role") != Some(&json!("developer")))
        .map(|i| (i["role"].as_str().unwrap().to_string(), i["content"][0]["text"].as_str().unwrap().to_string()))
        .collect();
    let pair = |r: &str, t: &str| (r.to_string(), t.to_string());
    assert_eq!(shown, vec![pair("user", "what's for dinner?"), pair("assistant", "Pasta tonight."), pair("user", "so what's the plan?")]);
    let everything = serde_json::to_string(&last).unwrap();
    for leaked in ["spllitter", "Ultrathink", "claude off", "stealth"] {
        assert!(!everything.contains(leaked), "{leaked}");
    }
    // the transcript is still the whole record, each line under its kind
    let kept: Vec<(String, String, String)> = rows(&memory).into_iter().filter(|(_, kind, _)| kind == "relay" || kind == "command").collect();
    assert_eq!(
        kept,
        vec![
            row("owner", "relay", "The expense spllitter doesn't work"),
            row("owner", "relay", "claude: Ultrathink"),
            row("owner", "command", "claude off"),
            row("owner", "command", "stealth"),
        ]
    );
}

// ---- apps: a finished build arrives as the app's picture with its Open button ----------------------------------

#[tokio::test]
async fn test_a_picture_with_web_app_buttons_goes_as_one_multipart_send_photo() {
    let r = recorder(json!({}));
    let api = bot_over(&r);
    let caption = format!("✅ Habit Tracker is ready.\n{}", "c".repeat(2000));
    let buttons = vec![("Open Habit Tracker".to_string(), "https://quiet-owl.trycloudflare.com/apps/habit-tracker/".to_string())];
    api.send_photo_web_apps(OWNER, b"\xff\xd8jpeg-bytes", &caption, &buttons).await.unwrap();
    api.close().await;
    let seen = r.requests.lock();
    let request = &seen[0];
    let kind = request.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-type")).map(|(_, v)| v.clone()).unwrap_or_default();
    assert!(request.url.ends_with("/sendPhoto") && kind.starts_with("multipart/form-data"), "{kind}");
    let body = &request.body;
    assert!(find(body, b"\xff\xd8jpeg-bytes").is_some() && find(body, br#"filename="app.jpg""#).is_some());
    assert!(find(body, "✅ Habit Tracker is ready.".as_bytes()).is_some()); // the app's icon is kept
    assert!(find(body, "c".repeat(telegram::MAX_CAPTION_CHARS - 26).as_bytes()).is_some() && find(body, "c".repeat(2000).as_bytes()).is_none());
    let head = b"name=\"reply_markup\"\r\n\r\n";
    let start = find(body, head).unwrap() + head.len();
    let end = start + find(&body[start..], b"\r\n--").unwrap();
    let markup: Value = serde_json::from_slice(&body[start..end]).unwrap();
    assert_eq!(markup, json!({"inline_keyboard": [[{"text": "Open Habit Tracker", "web_app": {"url": "https://quiet-owl.trycloudflare.com/apps/habit-tracker/"}}]]}));
}

// ---- restarts ---------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_the_restart_line_never_holds_up_the_shutdown() {
    // Rust: `asyncio.wait_for` cannot be patched; the shutdown is timed instead, and must end within the restart line's
    // budget (TURN_RESTART_SECS) although Telegram never answers.
    let _iso = isolated();
    let mut api = Hooked::new(FakeApi::new(vec![vec![said("hello?", 9)]]));
    api.before_message = None;
    let stuck = Arc::new(StuckSend(api));
    let create = FakeCreate::new(vec![]);
    create.push(Scripted::Later(Box::new(|| futures::future::pending().boxed())));
    let rig = rig_over(stuck, create, |_| {});
    let inlet = rig.inlet.clone();
    let loop_task = tokio::spawn(async move { inlet.run().await });
    settle(200).await;
    let started = Instant::now();
    loop_task.abort();
    let _ = loop_task.await;
    rig.inlet.say_relay_probe().shutdown().await;
    let waited = started.elapsed().as_secs_f64();
    assert!(waited <= telegram::TURN_RESTART_SECS + 0.5 && telegram::TURN_RESTART_SECS <= 2.0, "{waited}");
}

/// `class Stuck(FakeApi)`: Telegram never answers a sendMessage.
struct StuckSend(Hooked);

#[async_trait]
impl TelegramApi for StuckSend {
    async fn get_me(&self) -> Result<Obj, telegram::BotApiError> {
        self.0.get_me().await
    }
    async fn get_updates(&self, offset: Option<i64>, timeout: i64) -> Result<Vec<Obj>, telegram::BotApiError> {
        self.0.get_updates(offset, timeout).await
    }
    async fn receive_image(&self, item: &eve_telegram_images::Attachment) -> Result<eve_telegram_images::ReceivedImage, telegram::ApiImageError> {
        self.0.receive_image(item).await
    }
    async fn send_message(&self, _chat_id: i64, _text: &str, _title: Option<&str>, _subtitle: Option<&str>, _buttons: &[String], _reply_to: i64, _force_reply: Option<&str>) -> Result<(), telegram::BotApiError> {
        futures::future::pending().await
    }
    async fn send_inline(&self, chat_id: i64, text: &str, keyboard: &[Vec<(String, String, String)>], title: Option<&str>, subtitle: Option<&str>) -> Result<i64, telegram::BotApiError> {
        self.0.send_inline(chat_id, text, keyboard, title, subtitle).await
    }
    async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, keyboard: Option<&[Vec<(String, String, String)>]>) -> Result<(), telegram::BotApiError> {
        self.0.edit_message(chat_id, message_id, text, title, subtitle, keyboard).await
    }
    async fn drop_buttons(&self, chat_id: i64, message_id: i64) -> Result<(), telegram::BotApiError> {
        self.0.drop_buttons(chat_id, message_id).await
    }
    async fn answer_callback(&self, query_id: &str, text: &str) -> Result<(), telegram::BotApiError> {
        self.0.answer_callback(query_id, text).await
    }
    async fn react(&self, chat_id: i64, message_id: i64, emoji: &str) -> Result<(), telegram::BotApiError> {
        self.0.react(chat_id, message_id, emoji).await
    }
    async fn send_photo(&self, chat_id: i64, path: &std::path::Path, caption: &str) -> Result<(), telegram::BotApiError> {
        self.0.send_photo(chat_id, path, caption).await
    }
    async fn send_document(&self, chat_id: i64, path: &std::path::Path, caption: &str) -> Result<(), telegram::BotApiError> {
        self.0.send_document(chat_id, path, caption).await
    }
    async fn typing(&self, chat_id: i64) -> Result<(), telegram::BotApiError> {
        self.0.typing(chat_id).await
    }
    async fn send_draft(&self, chat_id: i64, draft_id: i64, text: &str) -> Result<(), telegram::BotApiError> {
        self.0.send_draft(chat_id, draft_id, text).await
    }
    async fn send_web_apps(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), telegram::BotApiError> {
        self.0.send_web_apps(chat_id, text, buttons).await
    }
    async fn send_url_buttons(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), telegram::BotApiError> {
        self.0.send_url_buttons(chat_id, text, buttons).await
    }
    async fn send_photo_web_apps(&self, chat_id: i64, jpeg: &[u8], caption: &str, buttons: &[(String, String)]) -> Result<(), telegram::BotApiError> {
        self.0.send_photo_web_apps(chat_id, jpeg, caption, buttons).await
    }
    async fn set_commands(&self, commands: &[(String, String)], chat_id: i64) -> Result<(), telegram::BotApiError> {
        self.0.set_commands(commands, chat_id).await
    }
}

#[tokio::test]
async fn test_open_it_after_a_shared_link_carries_the_link_into_the_task() {
    // 2026-09-23 23:35: "Use Google search to open it up" came right after the owner shared a Maps link; the model
    // wrote only the second message's words as the goal. The link from the owner's earlier message now rides along.
    let _iso = isolated();
    let rig = Rig::new(
        FakeApi::new(vec![vec![said("Use Google search to open it up", 1)]]),
        FakeCreate::new(vec![call("start_task", json!({"goal": "Use Google search to open it up"}), "call_1")]),
        config(),
        |_| {},
    );
    rig.inlet.tt_set_turns(
        vec![
            ("user".to_string(), "What do u think of this https://maps.app.goo.gl/EXAMPLE".to_string()),
            ("eve".to_string(), "Nice route.".to_string()),
        ],
        vec!["say".to_string(), "say".to_string()],
    );
    run_rig(&rig, || async {
        let goal = rig.agents.last().goal.lock().clone().unwrap_or_default();
        assert!(goal.contains("https://maps.app.goo.gl/EXAMPLE"), "{goal}");
        assert!(goal.starts_with("Use Google search to open it up"));
        rig.agents.last().release.set();
    })
    .await;
}

// ---- a call's sentences ------------------------------------------------------------------------------------------

#[test]
fn test_split_for_call_and_the_sentence_stream() {
    assert_eq!(telegram::split_for_call("Sure.\n\u{1F4CE}\ncode 1234"), ("Sure.".to_string(), "code 1234".to_string()));
    assert_eq!(telegram::split_for_call("See https://a.example/x."), ("See https://a.example/x.".to_string(), "https://a.example/x".to_string()));
    let said: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let s = said.clone();
    let reader = telegram::SentenceStream::new(Arc::new(move |line: &str| s.lock().push(line.to_string())), 12);
    for piece in ["It's sunny", " today. Hi", "gh of 72. Lo", "w of 55.\n\u{1F4CE}\n", "72/55"] {
        reader.feed(piece);
    }
    reader.flush();
    assert_eq!(*said.lock(), vec!["It's sunny today.", "High of 72.", "Low of 55."]);
    assert!(reader.text.lock().ends_with("72/55"));
}
