//! test_telegram.py's typing, drafts, questions and inline buttons that the unit files did not carry (2396-2450,
//! 2729-2940, 3037-3090, 3317, 3369-3490, 3573), ported with their names and assertions.
//!
//! The inlet's private state the Python reads (`_typing`, `_typing_tasks`, `_draft_task`, `_pending_answer`,
//! `_pending_answer_chat`, `_pending_strict`) and the private methods it calls (`_keep_typing`, `_start_thinking`,
//! `_think_hard`, `_app_tool`, `_dispatch`, `_shutdown`) are the inlet's doc-hidden `tt_*` hooks and its
//! `say_relay_probe`.

mod more;
mod support;

use std::sync::Arc;

use async_trait::async_trait;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_brain::prelude::{Flag, Http, HttpError, HttpRequest, HttpResponse, HttpTransport, Sessions};
use eve_brain::telegram::{self as telegram, BotApi, TelegramApi, TelegramConfig};
use eve_composio_tools::{ComposioBridge, ComposioClient, ComposioConfig, ComposioError, ComposioSession};
use more::*;
use support::*;

const STRANGER: i64 = 666;
const REPO: &str = "/Users/g/repo";

/// `await asyncio.gather(*list(rig.inlet._jobs))` after a settle.
async fn jobs(rig: &Rig) {
    settle(200).await;
    rig.inlet.say_relay_probe().jobs().await;
}

/// `await dispatch(rig, text, update_id=n, uid=...)`.
async fn dispatch(rig: &Rig, text: &str, update_id: i64, uid: i64) {
    rig.inlet.say_relay_probe().dispatch(&Value::Object(update(update_id, text, uid, NOW, update_id)));
    rig.inlet.say_relay_probe().jobs().await;
}

/// `await tap(rig, key, message_id)`.
async fn tap(rig: &Rig, key: &str, message_id: i64) {
    rig.inlet.say_relay_probe().dispatch(&Value::Object(tap_update(90, message_id, key, OWNER, "q1")));
    jobs(rig).await;
}

/// `api.buttons[-1]`.
fn last_buttons(api: &FakeApi) -> Vec<String> {
    api.log.lock().buttons.last().cloned().unwrap_or_default()
}

fn last_title(api: &FakeApi) -> Option<String> {
    api.log.lock().titled.last().and_then(|t| t.0.clone())
}

// ---- an httpx.MockTransport recording (method, body) --------------------------------------------------------------

struct Recorder {
    calls: Mutex<Vec<(String, Value)>>,
    result: Box<dyn Fn(&str) -> Value + Send + Sync>,
}

impl Recorder {
    fn new(result: impl Fn(&str) -> Value + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { calls: Mutex::new(Vec::new()), result: Box::new(result) })
    }
}

#[async_trait]
impl HttpTransport for Recorder {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let method = request.url.rsplit('/').next().unwrap_or("").to_string();
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let result = (self.result)(&method);
        self.calls.lock().push((method, body));
        Ok(HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: serde_json::to_vec(&json!({"ok": true, "result": result})).unwrap(),
        })
    }
}

fn bot_over(recorder: &Arc<Recorder>) -> BotApi {
    let http: Http = recorder.clone();
    BotApi::with_http("123456:AAsecretTOKENvalue", http)
}

// ---- questions -------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_question_answer_is_bound_to_the_requesting_chat() {
    let _iso = isolated();
    let api = FakeApi::new(vec![]);
    let config = TelegramConfig { owner_ids: vec![OWNER, STRANGER], ..config() };
    let rig = Rig::new(api, FakeCreate::new(vec![]), config, |_| {});
    let inlet = rig.inlet.clone();
    let question = tokio::spawn(async move { inlet.tt_ask_user("Approve the command?", OWNER).await });
    settle(200).await;
    dispatch(&rig, "yes", 1, STRANGER).await;
    assert!(!question.is_finished());
    dispatch(&rig, "no", 2, OWNER).await;
    assert_eq!(question.await.unwrap(), "no");
    assert_eq!(rig.inlet.tt_pending_answer_chat(), None);
    rig.inlet.say_relay_probe().shutdown().await;
}

#[tokio::test]
async fn test_buttons_are_a_one_time_reply_keyboard_and_a_reaction_is_one_emoji() {
    // The real request bodies: buttons ride only the last piece, as a one-time reply keyboard whose taps send their
    // own text; a reaction is setMessageReaction with one emoji.
    let recorder = Recorder::new(|_| json!(true));
    let api = bot_over(&recorder);
    let text = vec!["word ".repeat(700); 3].join("\n\n");
    api.send_message(OWNER, &text, None, None, &["claude on eve".to_string(), "new claude".to_string()], 0, None).await.unwrap();
    api.react(OWNER, 77, telegram::TYPED_REACTION).await.unwrap();
    api.close().await;
    let calls = recorder.calls.lock().clone();
    let sends: Vec<&Value> = calls.iter().filter(|(m, _)| m == "sendMessage").map(|(_, b)| b).collect();
    assert!(sends.len() >= 2 && sends[..sends.len() - 1].iter().all(|b| b.get("reply_markup").is_none()));
    assert_eq!(
        sends.last().unwrap()["reply_markup"],
        json!({"keyboard": [[{"text": "claude on eve"}], [{"text": "new claude"}]], "one_time_keyboard": true, "resize_keyboard": true})
    );
    assert_eq!(
        calls.last().cloned().unwrap(),
        ("setMessageReaction".to_string(), json!({"chat_id": OWNER, "message_id": 77, "reaction": [{"type": "emoji", "emoji": telegram::TYPED_REACTION}]}))
    );
}

#[tokio::test]
async fn test_a_reaction_can_be_taken_off_again() {
    let recorder = Recorder::new(|_| json!(true));
    let api = bot_over(&recorder);
    api.react(OWNER, 77, "").await.unwrap();
    api.close().await;
    assert_eq!(*recorder.calls.lock(), vec![("setMessageReaction".to_string(), json!({"chat_id": OWNER, "message_id": 77, "reaction": []}))]);
}

// ---- typing and drafts -----------------------------------------------------------------------------------------

#[tokio::test]
async fn test_typing_stops_at_its_cap_when_the_end_is_never_seen() {
    let _iso = isolated();
    let api = FakeApi::new(vec![]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), |_| {});
    rig.inlet.tt_keep_typing(OWNER, "relay", telegram::TYPING_RELAY_SECS);
    rig.inlet.tt_keep_typing(OWNER, "relay", telegram::TYPING_RELAY_SECS); // the same reason again: no second loop
    settle(1000).await;
    assert_eq!(api.log.lock().typings.len(), (telegram::TYPING_RELAY_SECS / telegram::TYPING_EVERY_SECS).floor() as usize);
    assert!(rig.inlet.tt_typing(OWNER).is_none() && !rig.inlet.tt_typing_task(OWNER));
}

#[tokio::test]
async fn test_think_hard_keeps_its_own_typing_and_a_question_pauses_it() {
    let _iso = isolated();
    let release = Arc::new(Flag::new());
    let r = release.clone();
    let api = FakeApi::new(vec![]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), move |o| {
        o.thinker = Some(Arc::new(move |_question: String, _context: String| {
            let r = r.clone();
            async move {
                r.wait().await;
                Ok(obj(json!({"ok": true, "answer": "42"})))
            }
            .boxed()
        }));
    });
    let inlet = rig.inlet.clone();
    let i = inlet.clone();
    let job = tokio::spawn(async move { i.tt_think_hard("why", OWNER).await });
    settle(10).await; // a few ticks (the fake sleep is instant)
    assert!(inlet.tt_typing(OWNER).unwrap_or_default().contains(&"think".to_string()) && !api.log.lock().typings.is_empty());
    release.set();
    assert_eq!(job.await.unwrap()["answer"], json!("42"));
    assert!(inlet.tt_typing(OWNER).is_none());
    // a question to the owner is not work: no "typing…" under it, and it comes back with the answer
    inlet.tt_keep_typing(OWNER, "turn", telegram::TYPING_TURN_SECS);
    let i = inlet.clone();
    let ask = tokio::spawn(async move { i.tt_ask_user("Which one?", OWNER).await });
    settle(10).await;
    let waiting = api.log.lock().typings.len();
    settle(10).await;
    assert!(api.log.lock().typings.len() == waiting && inlet.tt_typing(OWNER).is_none());
    inlet.tt_pending_answer().expect("a question waits").set_result("the red one");
    assert_eq!(ask.await.unwrap(), "the red one");
    assert!(inlet.tt_typing(OWNER).unwrap_or_default().contains(&"turn".to_string()));
    inlet.tt_stop_typing(OWNER, "turn");
}

/// `_relay_rig(api, refresh=True)`: the relay joined to one session, every sleep instant.
fn refreshing_relay_rig(api: Arc<FakeApi>) -> Rig {
    Rig::new(api, FakeCreate::new(vec![]), config(), |o| {
        o.terminal = Some(Arc::new(|_cwd: String, _text: String| async { Ok(String::new()) }.boxed()));
        o.claude_sessions = Some(Arc::new(|| Ok(Sessions::Now(vec![REPO.to_string()]))));
        o.sleep = Some(Arc::new(|_secs: f64| async { tokio::task::yield_now().await }.boxed()));
    })
}

#[tokio::test]
async fn test_the_thinking_draft_stops_at_its_cap_when_the_turn_end_is_never_seen() {
    let _iso = isolated();
    let api = FakeApi::new(vec![]);
    let rig = refreshing_relay_rig(api.clone());
    rig.inlet.tt_start_thinking(OWNER);
    rig.inlet.tt_start_thinking(OWNER); // again: one loop, not two
    settle(1000).await;
    let drafts = api.log.lock().drafts.clone();
    assert_eq!(drafts.len(), (telegram::TYPING_RELAY_SECS / telegram::DRAFT_REFRESH_SECS).ceil() as usize);
    assert!(drafts.iter().all(|d| d.1 == 1) && !rig.inlet.tt_draft_task());
}

// ---- inline buttons: callbacks, Allow/Deny, options, pickers (owner, 2026-09-23) --------------------------------

#[test]
fn test_a_tap_is_accepted_only_from_the_owner_in_their_private_chat() {
    let cfg = config();
    let v = |o: Obj| Value::Object(o);
    let (ok, got) = telegram::accept_tap(&v(tap_update(90, 7, "k.1", OWNER, "q1")), &cfg);
    assert_eq!(ok, telegram::OK);
    assert_eq!(got, Some(telegram::Tap { query_id: "q1".into(), chat_id: OWNER, user_id: OWNER, data: "k.1".into(), message_id: 7 }));
    assert_eq!(telegram::accept_tap(&v(tap_update(90, 1001, "k.1", STRANGER, "q1")), &cfg), ("stranger".to_string(), None));
    let mut group = tap_update(90, 1001, "k.1", OWNER, "q1");
    group["callback_query"]["message"]["chat"] = json!({"id": -100, "type": "group"});
    assert_eq!(telegram::accept_tap(&v(group), &cfg), ("not-private".to_string(), None));
    let mut bot = tap_update(90, 1001, "k.1", OWNER, "q1");
    bot["callback_query"]["from"]["is_bot"] = json!(true);
    assert_eq!(telegram::accept_tap(&v(bot), &cfg), ("bot".to_string(), None));
    assert_eq!(telegram::accept_tap(&json!({"callback_query": {"id": 5}}), &cfg).0, "malformed");
    assert_eq!(telegram::accept_tap(&v(update(1, "hi", OWNER, NOW, 1)), &cfg), ("not-a-tap".to_string(), None));
    let mut game = tap_update(90, 1001, "k.1", OWNER, "q1");
    game["callback_query"].as_object_mut().unwrap().remove("data");
    assert_eq!(telegram::accept_tap(&v(game), &cfg).1.unwrap().data, ""); // answered "expired", never acted on
    assert_eq!(telegram::sender_id(&v(tap_update(90, 1001, "k.1", STRANGER, "q1"))), Some(STRANGER));
}

#[tokio::test]
async fn test_the_real_requests_poll_for_taps_send_inline_keys_edit_and_answer() {
    let recorder = Recorder::new(|method| if method == "getUpdates" { json!([]) } else { json!({"message_id": 77}) });
    let api = bot_over(&recorder);
    api.get_updates(None, 0).await.unwrap();
    let keyboard = vec![vec![
        ("Allow".to_string(), "a1b2c3.1".to_string(), "success".to_string()),
        ("Deny".to_string(), "a1b2c3.2".to_string(), "danger".to_string()),
    ]];
    let sent = api.send_inline(OWNER, "rm -rf build?", &keyboard, Some("Claude asks"), None).await.unwrap();
    api.edit_message(OWNER, 77, "rm -rf build?\n\nAllowed.", Some("Claude asks"), None, None).await.unwrap();
    api.drop_buttons(OWNER, 78).await.unwrap();
    api.answer_callback("q9", &"Allowed.".repeat(40)).await.unwrap();
    api.close().await;
    assert_eq!(sent, 77);
    let methods: Map<String, Value> = recorder.calls.lock().iter().cloned().collect();
    assert_eq!(methods["getUpdates"]["allowed_updates"], json!(["message", "callback_query"]));
    assert_eq!(
        methods["sendMessage"]["reply_markup"],
        json!({"inline_keyboard": [[{"text": "Allow", "callback_data": "a1b2c3.1", "style": "success"},
                                    {"text": "Deny", "callback_data": "a1b2c3.2", "style": "danger"}]]})
    );
    assert_eq!(methods["editMessageText"]["reply_markup"], json!({"inline_keyboard": []}));
    assert!(methods["editMessageText"]["message_id"] == json!(77) && methods["editMessageText"]["text"].as_str().unwrap().contains("Allowed."));
    assert_eq!(methods["editMessageReplyMarkup"], json!({"chat_id": OWNER, "message_id": 78, "reply_markup": {"inline_keyboard": []}}));
    assert_eq!(methods["answerCallbackQuery"]["callback_query_id"], json!("q9"));
    assert_eq!(methods["answerCallbackQuery"]["text"].as_str().unwrap().chars().count(), telegram::MAX_TOAST_CHARS);
}

#[test]
fn test_only_questions_with_known_answers_get_buttons() {
    let cases: [(&str, &[&str]); 8] = [
        (
            "Codex: Allow Calendar?\nReply: yes to allow once; \"allow for task\"; \"always allow\" to remember this app for future tasks; no to deny.",
            &["Allow once", "Allow for this task", "Always allow", "Deny"],
        ),
        ("Codex: Allow Calendar?\nReply: yes to allow once; no to deny.", &["Allow once", "Deny"]),
        ("Codex asks to run this command in /r:\nls\n\nReply yes or no.", &["Allow", "Deny"]),
        ("Create the event Lunch?\n\nyes / no?", &["Allow", "Deny"]),
        ("eve wants to control your Chrome. Allow it? yes / no", &["Allow", "Deny"]),
        ("Should I go ahead: press Buy now?", &["Yes", "No"]),
        ("Which account, work or personal?", &[]),
        ("Codex: Pick one\nReply: blue; red.", &[]),
    ];
    for (question, labels) in cases {
        let got: Vec<String> = telegram::answer_choices(question).into_iter().map(|c| c.label).collect();
        assert_eq!(got, labels.iter().map(|l| l.to_string()).collect::<Vec<_>>(), "{question}");
    }
}

// ---- Composio's consent ----------------------------------------------------------------------------------------

/// `ConsentApps`: a Composio stand-in that records what ran (the real bridge over a session that answers ok).
struct ConsentSession {
    ran: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ComposioSession for ConsentSession {
    fn session_id(&self) -> String {
        "s1".into()
    }
    async fn tools(&self) -> Result<Vec<Value>, ComposioError> {
        Ok(vec![json!({"type": "function", "name": "COMPOSIO_MULTI_EXECUTE_TOOL", "parameters": {"type": "object"}})])
    }
    async fn execute(&self, slug: &str, _arguments: &Map<String, Value>) -> Result<Value, ComposioError> {
        self.ran.lock().push(slug.to_string());
        Ok(json!({"ok": true}))
    }
}

struct ConsentClient {
    ran: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl ComposioClient for ConsentClient {
    async fn create_session(&self, _user_id: &str, _toolkits: &Value, _multi: Option<&Value>) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        Ok(Arc::new(ConsentSession { ran: self.ran.clone() }))
    }
    async fn use_session(&self, _session_id: &str) -> Result<Arc<dyn ComposioSession>, ComposioError> {
        Ok(Arc::new(ConsentSession { ran: self.ran.clone() }))
    }
    async fn list_connected_accounts(&self, _user_ids: &[String]) -> Result<Vec<Value>, ComposioError> {
        Ok(Vec::new())
    }
    async fn execute_tool(&self, _slug: &str, _arguments: &Map<String, Value>, _user_id: &str, _account: &str) -> Result<Value, ComposioError> {
        Err(ComposioError::new("AssertionError", "not in these tests"))
    }
}

struct ConsentApps {
    bridge: Arc<ComposioBridge>,
    ran: Arc<Mutex<Vec<String>>>,
    _dir: tempfile::TempDir,
}

async fn consent_apps() -> ConsentApps {
    let dir = tmp();
    let ran = Arc::new(Mutex::new(Vec::new()));
    let r = ran.clone();
    let mut config = ComposioConfig::new(true, "owner");
    config.state_path = dir.path().join("composio.json");
    config.toolkits = None;
    let bridge = ComposioBridge::new(config, Some(Arc::new(move || Ok(Arc::new(ConsentClient { ran: r.clone() }) as Arc<dyn ComposioClient>))));
    bridge.start().await.unwrap();
    ConsentApps { bridge: Arc::new(bridge), ran, _dir: dir }
}

fn drive_delete() -> Obj {
    obj(json!({"tools": [{"tool_slug": "GOOGLEDRIVE_DELETE_FILE", "arguments": {"file_id": "x"}}]}))
}

/// `relay_rig(api, typed, apps=apps)`: the relay joined to one session in the owner's chat, asking permissions, each
/// line typed to Claude recorded.
fn relay_rig(api: Arc<dyn TelegramApi>, typed: &Arc<Mutex<Vec<String>>>, apps: &ConsentApps) -> Rig {
    let t = typed.clone();
    let bridge = apps.bridge.clone();
    let rig = rig_over_config(api, FakeCreate::new(vec![]), TelegramConfig { ask_permissions: true, ..config() }, move |o| {
        o.terminal = Some(Arc::new(move |_cwd: String, text: String| {
            t.lock().push(text);
            async { Ok(String::new()) }.boxed()
        }));
        o.claude_sessions = Some(Arc::new(|| Ok(Sessions::Now(vec![REPO.to_string()]))));
        o.apps = Some(bridge);
    });
    rig.inlet.tt_relay_to(REPO);
    rig.inlet.tt_set_chat_id(Some(OWNER));
    rig
}

#[tokio::test]
async fn test_composio_asks_with_allow_and_deny_and_a_tap_decides() {
    let _iso = isolated();
    let apps = consent_apps().await;
    let api = FakeApi::new(vec![]);
    let bridge = apps.bridge.clone();
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), move |o| o.apps = Some(bridge));
    let args = drive_delete();
    let inlet = rig.inlet.clone();
    let result = tokio::spawn(async move { inlet.tt_app_tool("COMPOSIO_MULTI_EXECUTE_TOOL", &args, OWNER).await });
    jobs(&rig).await;
    assert!(last_buttons(&api) == vec!["Allow", "Deny"] && last_title(&api).as_deref() == Some(telegram::APP_ASKS_TITLE));
    let (message_id, key) = api.key("Allow", -1);
    tap(&rig, &key, message_id).await;
    assert!(Value::Object(result.await.unwrap()) == json!({"ok": true}) && *apps.ran.lock() == vec!["COMPOSIO_MULTI_EXECUTE_TOOL"]);
    rig.inlet.say_relay_probe().shutdown().await;
}

#[tokio::test]
async fn test_while_a_composio_consent_waits_a_sentence_for_claude_goes_to_claude_and_nothing_runs() {
    // Review finding: _ask_user's Allow/Deny prompts were not strict, so with the relay on the owner's next message
    // for Claude was eaten as the consent, and "ok, also update the README" ran a Drive delete.
    let _iso = isolated();
    let (api, typed, apps) = (FakeApi::new(vec![]), Arc::new(Mutex::new(Vec::new())), consent_apps().await);
    let rig = relay_rig(api.clone(), &typed, &apps);
    let inlet = rig.inlet.clone();
    let result = tokio::spawn(async move { inlet.tt_app_tool("COMPOSIO_MULTI_EXECUTE_TOOL", &drive_delete(), OWNER).await });
    jobs(&rig).await;
    assert_eq!(last_buttons(&api), vec!["Allow", "Deny"]);
    dispatch(&rig, "ok, also update the README", 5, OWNER).await;
    assert!(*typed.lock() == vec!["ok, also update the README"] && !result.is_finished() && apps.ran.lock().is_empty());
    dispatch(&rig, "no", 6, OWNER).await; // a bare no still answers by typing
    assert!(result.await.unwrap()["ok"] == json!(false) && apps.ran.lock().is_empty());
    // and a bare yes, typed, allows as a tap would
    let inlet = rig.inlet.clone();
    let result = tokio::spawn(async move { inlet.tt_app_tool("COMPOSIO_MULTI_EXECUTE_TOOL", &drive_delete(), OWNER).await });
    jobs(&rig).await;
    dispatch(&rig, "yes", 7, OWNER).await;
    assert!(Value::Object(result.await.unwrap()) == json!({"ok": true}) && *typed.lock() == vec!["ok, also update the README"]);
    rig.inlet.say_relay_probe().shutdown().await;
}

#[tokio::test]
async fn test_text_sent_while_a_composio_consent_is_still_on_its_way_goes_to_claude() {
    // Strict from creation, as decide_permission is: a message typed during the send is not the answer.
    let _iso = isolated();
    let inner = FakeApi::new(vec![]);
    let gate = Arc::new(Flag::new());
    let sending = Arc::new(Flag::new());
    let mut slow = Hooked::new(inner.clone());
    let (g, s) = (gate.clone(), sending.clone());
    slow.before_inline = Some(Box::new(move || {
        let (g, s) = (g.clone(), s.clone());
        async move {
            s.set();
            g.wait().await;
        }
        .boxed()
    }));
    let (typed, apps) = (Arc::new(Mutex::new(Vec::new())), consent_apps().await);
    let rig = relay_rig(Arc::new(slow), &typed, &apps);
    let inlet = rig.inlet.clone();
    let result = tokio::spawn(async move { inlet.tt_app_tool("COMPOSIO_MULTI_EXECUTE_TOOL", &drive_delete(), OWNER).await });
    sending.wait().await;
    dispatch(&rig, "ok, also update the README", 5, OWNER).await;
    assert!(*typed.lock() == vec!["ok, also update the README"] && !result.is_finished());
    gate.set();
    jobs(&rig).await;
    let (message_id, key) = inner.key("Deny", -1);
    tap(&rig, &key, message_id).await;
    assert!(result.await.unwrap()["ok"] == json!(false) && apps.ran.lock().is_empty());
    rig.inlet.say_relay_probe().shutdown().await;
}

#[tokio::test]
async fn test_a_composio_consent_whose_buttons_fail_is_answered_by_the_next_message_as_before() {
    let _iso = isolated();
    let api = FakeApi::new(vec![]);
    *api.fail_inline.lock() = Some(api_error(400, "Bad Request: BUTTON_DATA_INVALID"));
    let (typed, apps) = (Arc::new(Mutex::new(Vec::new())), consent_apps().await);
    let rig = relay_rig(api.clone(), &typed, &apps);
    let inlet = rig.inlet.clone();
    let result = tokio::spawn(async move { inlet.tt_app_tool("COMPOSIO_MULTI_EXECUTE_TOOL", &drive_delete(), OWNER).await });
    jobs(&rig).await;
    assert!(api.log.lock().buttons.last().unwrap().is_empty() && !rig.inlet.say_relay_probe().pending().0);
    // Firm since 2026-10-05 (review): with no buttons, a bare yes or no typed is still its answer, and other words go
    // on to where they were meant (here the relay), never read as a yes or a no
    dispatch(&rig, "not now", 5, OWNER).await;
    assert!(*typed.lock() == vec!["not now"] && !result.is_finished() && apps.ran.lock().is_empty());
    dispatch(&rig, "no", 6, OWNER).await;
    assert!(result.await.unwrap()["ok"] == json!(false) && *typed.lock() == vec!["not now"] && apps.ran.lock().is_empty());
    rig.inlet.say_relay_probe().shutdown().await;
}

#[test]
fn test_question_options_are_read_only_from_one_numbered_question() {
    assert_eq!(telegram::question_options("Which? (1. A / B / 2. C)"), vec![(1, "A / B".to_string()), (2, "C".to_string())]);
    assert_eq!(telegram::question_options("Which? (1. A)"), vec![(1, "A".to_string())]);
    assert!(telegram::question_options("Which? (2. A / 3. B)").is_empty());
    assert!(telegram::question_options("Just a question?").is_empty());
}
