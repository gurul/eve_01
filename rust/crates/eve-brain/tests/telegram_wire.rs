//! Unit `telegram_wire`: telegram.py 1469-2447 and 7594-7699 (accept, brain, bot_api, make); tests: test_telegram.py the poll loop (1457-1517), the / menu, typing, drafts (2481-3012), no stale typing and truthful endings (4382-4707); test_telegram_app_only.py.
//!
//! The shared fakes are in `support` (architect-owned); this unit's own go here. The tests that read the inlet's
//! private state (its typing reasons, its drafts, its questions and progress messages) cannot reach it from here:
//! they are in `src/telegram/make.rs`'s test module, with a fake of their own.

mod support;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_brain::prelude::{Flag, Http, HttpError, HttpRequest, HttpResponse, HttpTransport, Lent, Sessions};
use eve_brain::telegram::*;
use support::*;

// ---- what every test here shares ------------------------------------------------------------------------------

/// The spend ledger and the hidden tokens are process-wide: the tests that touch them (every inlet run, every
/// token) take this lock, and each gets a ledger folder of its own, never the owner's real one
/// (conftest.py's `_spend_ledger_in_tmp`).
static SERIAL: Mutex<()> = Mutex::new(());

struct Isolated {
    _lock: parking_lot::MutexGuard<'static, ()>,
    ledger: tempfile::TempDir,
}

impl Drop for Isolated {
    fn drop(&mut self) {
        eve_spend::set_dir(None);
        unhide_tokens();
    }
}

fn isolated() -> Isolated {
    let lock = SERIAL.lock();
    let ledger = tmp();
    eve_spend::set_dir(Some(ledger.path().to_path_buf()));
    Isolated { _lock: lock, ledger }
}

const TOKEN: &str = "123456:AAsecretTOKENvalue";

fn obj(v: Value) -> Obj {
    v.as_object().cloned().expect("an object")
}

fn val(o: &Obj) -> Value {
    Value::Object(o.clone())
}

/// `update(text, update_id=n)` from the owner, now.
fn said(text: &str, update_id: i64) -> Obj {
    update(update_id, text, OWNER, NOW, update_id)
}

/// test_telegram.py's `run_rig`: the poll loop runs, `during` runs while it does, then it is stopped. The loop must
/// still be running when it is stopped.
async fn run_rig<F, Fut>(rig: &Rig, during: F)
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let inlet = rig.inlet.clone();
    let loop_task = tokio::spawn(async move { inlet.run().await });
    settle(200).await;
    during().await;
    settle(200).await;
    assert!(!loop_task.is_finished(), "the poll loop died");
    loop_task.abort();
    let _ = loop_task.await;
}

// ---- an httpx.MockTransport ------------------------------------------------------------------------------------

type Handler = Box<dyn Fn(&HttpRequest) -> Result<HttpResponse, HttpError> + Send + Sync>;

/// Every request the client made, answered by `handler`.
struct MockTransport {
    handler: Handler,
    requests: Mutex<Vec<HttpRequest>>,
}

impl MockTransport {
    fn new(handler: impl Fn(&HttpRequest) -> Result<HttpResponse, HttpError> + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { handler: Box::new(handler), requests: Mutex::new(Vec::new()) })
    }

    /// Always `{"ok": true, "result": result}`.
    fn ok(result: Value) -> Arc<Self> {
        Self::new(move |_| Ok(respond(200, &json!({"ok": true, "result": result}))))
    }

    /// (method, JSON body) per request, as test_telegram.py's handlers record them.
    fn calls(&self) -> Vec<(String, Value)> {
        self.requests
            .lock()
            .iter()
            .map(|r| (r.url.rsplit('/').next().unwrap_or("").to_string(), serde_json::from_slice(&r.body).unwrap_or(Value::Null)))
            .collect()
    }
}

#[async_trait]
impl HttpTransport for MockTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        let answer = (self.handler)(&request);
        self.requests.lock().push(request);
        answer
    }
}

fn respond(status: u16, body: &Value) -> HttpResponse {
    HttpResponse { status, headers: vec![("content-type".into(), "application/json".into())], body: serde_json::to_vec(body).unwrap() }
}

fn bot(transport: &Arc<MockTransport>) -> BotApi {
    let http: Http = transport.clone();
    BotApi::with_http(TOKEN, http)
}

/// FakeApi with one behaviour changed: a poll fails the test, Telegram refuses the menu, or "typing…" is refused.
#[derive(Default)]
struct Odd {
    never_poll: bool,
    no_menu: bool,
    no_typing: bool,
}

struct Wrapped(Arc<FakeApi>, Odd);

#[async_trait]
impl TelegramApi for Wrapped {
    async fn get_me(&self) -> Result<Obj, BotApiError> {
        self.0.get_me().await
    }
    async fn get_updates(&self, offset: Option<i64>, timeout: i64) -> Result<Vec<Obj>, BotApiError> {
        assert!(!self.1.never_poll, "polled Telegram in app-only mode");
        self.0.get_updates(offset, timeout).await
    }
    async fn receive_image(&self, item: &eve_telegram_images::Attachment) -> Result<eve_telegram_images::ReceivedImage, ApiImageError> {
        self.0.receive_image(item).await
    }
    async fn send_message(&self, chat_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, buttons: &[String], reply_to: i64, force_reply: Option<&str>) -> Result<(), BotApiError> {
        self.0.send_message(chat_id, text, title, subtitle, buttons, reply_to, force_reply).await
    }
    async fn send_inline(&self, chat_id: i64, text: &str, keyboard: &[Vec<(String, String, String)>], title: Option<&str>, subtitle: Option<&str>) -> Result<i64, BotApiError> {
        self.0.send_inline(chat_id, text, keyboard, title, subtitle).await
    }
    async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, keyboard: Option<&[Vec<(String, String, String)>]>) -> Result<(), BotApiError> {
        self.0.edit_message(chat_id, message_id, text, title, subtitle, keyboard).await
    }
    async fn drop_buttons(&self, chat_id: i64, message_id: i64) -> Result<(), BotApiError> {
        self.0.drop_buttons(chat_id, message_id).await
    }
    async fn answer_callback(&self, query_id: &str, text: &str) -> Result<(), BotApiError> {
        self.0.answer_callback(query_id, text).await
    }
    async fn react(&self, chat_id: i64, message_id: i64, emoji: &str) -> Result<(), BotApiError> {
        self.0.react(chat_id, message_id, emoji).await
    }
    async fn send_photo(&self, chat_id: i64, path: &std::path::Path, caption: &str) -> Result<(), BotApiError> {
        self.0.send_photo(chat_id, path, caption).await
    }
    async fn send_document(&self, chat_id: i64, path: &std::path::Path, caption: &str) -> Result<(), BotApiError> {
        self.0.send_document(chat_id, path, caption).await
    }
    async fn typing(&self, chat_id: i64) -> Result<(), BotApiError> {
        if self.1.no_typing {
            self.0.log.lock().typings.push(chat_id);
            return Err(api_error(429, "Too Many Requests: retry after 5"));
        }
        self.0.typing(chat_id).await
    }
    async fn send_draft(&self, chat_id: i64, draft_id: i64, text: &str) -> Result<(), BotApiError> {
        self.0.send_draft(chat_id, draft_id, text).await
    }
    async fn send_web_apps(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.0.send_web_apps(chat_id, text, buttons).await
    }
    async fn send_url_buttons(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.0.send_url_buttons(chat_id, text, buttons).await
    }
    async fn send_photo_web_apps(&self, chat_id: i64, jpeg: &[u8], caption: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.0.send_photo_web_apps(chat_id, jpeg, caption, buttons).await
    }
    async fn set_commands(&self, commands: &[(String, String)], chat_id: i64) -> Result<(), BotApiError> {
        if self.1.no_menu {
            return Err(api_error(400, "Bad Request: BOT_COMMAND_INVALID"));
        }
        self.0.set_commands(commands, chat_id).await
    }
}


// ---- accept: who may speak --------------------------------------------------------------------------------------

#[test]
fn accept_takes_the_owners_own_words_and_drops_everyone_else() {
    let cfg = config();
    let (verdict, inbound) = accept(&val(&said("  hello  ", 5)), &cfg, NOW);
    assert_eq!(verdict, OK);
    let inbound = inbound.unwrap();
    assert_eq!((inbound.chat_id, inbound.user_id, inbound.text.as_str(), inbound.message_id), (OWNER, OWNER, "hello", 5));
    assert_eq!(accept(&val(&update(1, "hi", 666, NOW, 1)), &cfg, NOW).0, "stranger");
    assert_eq!(accept(&val(&said("hi", 1)), &cfg, NOW + cfg.stale_secs + 1.0).0, "stale");
    assert_eq!(accept(&json!("not a dict"), &cfg, NOW).0, "malformed");
    assert_eq!(accept(&json!({"update_id": 1, "edited_message": {}}), &cfg, NOW).0, "not-a-new-message");
    let mut group = said("hi", 1);
    group["message"]["chat"] = json!({"id": -100, "type": "group"});
    assert_eq!(accept(&val(&group), &cfg, NOW).0, "not-private");
    let mut robot = said("hi", 1);
    robot["message"]["from"]["is_bot"] = json!(true);
    assert_eq!(accept(&val(&robot), &cfg, NOW).0, "bot");
    let mut boolean = said("hi", 1);
    boolean["message"]["from"]["id"] = json!(true);
    assert_eq!(accept(&val(&boolean), &cfg, NOW).0, "malformed"); // a bool is not a user id
}

#[test]
fn accept_says_what_the_owner_sent_that_is_not_words() {
    let cfg = config();
    let mut forwarded = said("look", 3);
    forwarded["message"]["forward_date"] = json!(1);
    let (verdict, inbound) = accept(&val(&forwarded), &cfg, NOW);
    assert_eq!(verdict, FORWARDED);
    assert_eq!(inbound.unwrap().text, "");
    let mut sticker = said("x", 4);
    sticker["message"].as_object_mut().unwrap().remove("text");
    sticker["message"]["sticker"] = json!({"file_id": "s"});
    assert_eq!(accept(&val(&sticker), &cfg, NOW).0, NOT_TEXT);
    let mut photo = said("x", 6);
    photo["message"].as_object_mut().unwrap().remove("text");
    photo["message"]["photo"] = json!([{"file_id": "small", "file_size": 10}, {"file_id": "big", "file_size": 99}]);
    photo["message"]["caption"] = json!("  what is this?  ");
    let (verdict, inbound) = accept(&val(&photo), &cfg, NOW);
    let inbound = inbound.unwrap();
    assert_eq!(verdict, OK);
    assert_eq!(inbound.text, "what is this?");
    assert_eq!(inbound.image.unwrap().file_id, "big");
}

#[test]
fn accept_tap_takes_only_the_owners_tap_in_their_own_chat() {
    let cfg = config();
    let (verdict, tap) = accept_tap(&val(&tap_update(90, 1001, "k.1", OWNER, "q1")), &cfg);
    assert_eq!(verdict, OK);
    assert_eq!(tap.unwrap(), Tap { query_id: "q1".into(), chat_id: OWNER, user_id: OWNER, data: "k.1".into(), message_id: 1001 });
    assert_eq!(accept_tap(&val(&tap_update(90, 1001, "k.1", 666, "q1")), &cfg).0, "stranger");
    let mut group = tap_update(90, 1001, "k.1", OWNER, "q1");
    group["callback_query"]["message"]["chat"] = json!({"id": -100, "type": "group"});
    assert_eq!(accept_tap(&val(&group), &cfg).0, "not-private");
    assert_eq!(accept_tap(&val(&said("hi", 1)), &cfg).0, "not-a-tap");
    let mut no_id = tap_update(90, 1001, "k.1", OWNER, "q1");
    no_id["callback_query"]["id"] = json!(5);
    assert_eq!(accept_tap(&val(&no_id), &cfg).0, "malformed");
    // Python's ==: a chat id of 42.0 is the owner's 42, and 43.0 is not
    let mut float_chat = tap_update(90, 1001, "k.1", OWNER, "q1");
    float_chat["callback_query"]["message"]["chat"]["id"] = json!(OWNER as f64);
    assert_eq!(accept_tap(&val(&float_chat), &cfg).0, OK);
    float_chat["callback_query"]["message"]["chat"]["id"] = json!(OWNER as f64 + 1.0);
    assert_eq!(accept_tap(&val(&float_chat), &cfg).0, "not-private");
}

#[test]
fn sender_id_names_the_number_only() {
    assert_eq!(sender_id(&val(&said("hi", 1))), Some(OWNER));
    assert_eq!(sender_id(&val(&tap_update(1, 2, "k", 77, "q"))), Some(77));
    assert_eq!(sender_id(&json!({"update_id": 1})), None);
}

// ---- buttons and their words ------------------------------------------------------------------------------------

#[test]
fn a_question_gets_buttons_only_when_its_answers_are_known() {
    assert_eq!(answer_choices("Open Chrome? yes / no?"), ALLOW_DENY.clone());
    assert_eq!(answer_choices("Should I go ahead: send it?"), YES_NO.clone());
    assert!(answer_choices("Which account?").is_empty());
    let codex = answer_choices("Codex wants Safari.\nReply: yes to allow once; \"always allow\"; no to deny.");
    assert_eq!(codex.iter().map(|c| c.value.as_str()).collect::<Vec<_>>(), ["yes", "always allow", "no"]);
    assert!(answer_choices("Codex wants Safari.\nReply: maybe later.").is_empty()); // never a guess
    // each piece is str.strip()ped, which takes Python's whitespace (\x1f is one) off its ends
    let spaced = answer_choices("Codex wants Safari.\nReply: yes to allow once;\u{1f}no to deny.");
    assert_eq!(spaced.iter().map(|c| c.value.as_str()).collect::<Vec<_>>(), ["yes", "no"]);
    assert_eq!(reply_words(&ALLOW_DENY), "Reply \"yes\" or \"no\".");
}

#[test]
fn a_relayed_question_is_one_question_with_numbered_options() {
    assert_eq!(question_options("Ship now? (1. Yes / 2. No)"), vec![(1, "Yes".to_string()), (2, "No".to_string())]);
    assert!(question_options("Ship now? (1. Yes / 2. No) | Tag it? (1. Yes)").is_empty());
    assert!(question_options("Ship now? (1. Yes / 3. No)").is_empty());
    // the split's lookahead consumes nothing: an empty option between two cuts refuses the lot, as in Python
    assert!(question_options("Pick one (1. a / 2. / 3. b)").is_empty());
    assert_eq!(question_options("Pick one (1. a / b / 2. c)"), vec![(1, "a / b".to_string()), (2, "c".to_string())]);
    assert!(question_options("Ship now?").is_empty());
}

#[test]
fn a_spoken_question_says_how_to_answer() {
    assert_eq!(spoken_question("Open it?", &ALLOW_DENY), "Open it? Yes or no?");
    let pick = [Choice::plain("📁 eve", "eve"), Choice::plain("Other (new)", "other")];
    assert_eq!(spoken_question("Which?", &pick), "Which? Say eve, or Other new.");
}

#[test]
fn a_receipt_line_and_a_tool_line() {
    assert_eq!(receipt_line("remember", &Obj::new()), STARRED_LINE);
    assert_eq!(receipt_line("take_notes", &obj(json!({"already": true}))), "Already taking notes.");
    assert_eq!(receipt_line("take_notes", &Obj::new()), NOTES_LINE);
    assert_eq!(receipt_line("capture", &obj(json!({"line": "Saved to Inbox."}))), "Saved to Inbox.");
    assert_eq!(receipt_line("capture", &Obj::new()), "Saved.");
}

// ---- the request and its parse -------------------------------------------------------------------------------------

#[test]
fn the_turn_note_sits_right_before_the_newest_user_message() {
    let items = vec![message_item("user", "one"), message_item("assistant", "two"), message_item("user", "three")];
    let out = with_turn_context(&items, "the clock");
    assert_eq!(out.len(), 4);
    assert_eq!(out[2], message_item("developer", "the clock"));
    assert_eq!(out[3], items[2]);
    assert_eq!(with_turn_context(&items, ""), items);
    assert_eq!(with_turn_context(&[], "note"), vec![message_item("developer", "note")]);
    assert_eq!(message_item("assistant", "x")["content"][0]["type"], "output_text");
    let note = turn_context("  back   from lunch ", Some(" 12:00 "));
    assert_eq!(note, format!("12:00{BRIEF_HEADER}back from lunch{BRIEF_RULES}"));
    assert_eq!(turn_context("", Some("12:00")), "12:00");
}

#[test]
fn a_graded_round_keeps_the_prefix_and_changes_the_model() {
    let cfg = config();
    let payload = obj(json!({"model": "m", "instructions": "i", "reasoning": {"effort": "high"},
                             "tools": [{"name": "think_hard"}, {"name": "web_search"}]}));
    let instant = graded(&payload, &cfg, "instant");
    assert_eq!(instant["model"], json!(cfg.model));
    assert_eq!(instant["reasoning"], json!({"effort": "low"}));
    assert_eq!(instant["tools"], json!([{"name": "web_search"}]));
    let think = graded(&payload, &cfg, "think");
    assert_eq!(think["model"], json!(cfg.app_think_model));
    assert_eq!(think["reasoning"], json!({"effort": cfg.app_think_effort}));
    assert_eq!(think["tools"], payload["tools"]);
    assert_eq!(graded(&payload, &cfg, "auto"), payload);
}

#[test]
fn parse_response_reads_calls_text_and_what_goes_back() {
    let response = json!({"output": [
        {"type": "reasoning", "id": "rs_1", "encrypted_content": "opaque"},
        {"type": "function_call", "name": "start_task", "call_id": "c1", "arguments": "{\"goal\": \"open it\"}"},
        {"type": "message", "content": [{"type": "output_text", "text": " Here ([](https://x)) you go. "},
                                        {"type": "refusal", "refusal": ""}]}]});
    let (calls, text, carry) = parse_response(&response, &[], false).unwrap();
    assert_eq!(calls, vec![obj(json!({"name": "start_task", "call_id": "c1", "args": {"goal": "open it"}}))]);
    assert_eq!(text, "Here you go.\nI can't do that.");
    assert_eq!(carry.len(), 2);
    assert!(parse_response(&json!({"output": "no"}), &[], false).is_err());
    let unknown = json!({"output": [{"type": "function_call", "name": "rm_rf", "call_id": "c2", "arguments": ""}]});
    let error = parse_response(&unknown, &[], false).unwrap_err();
    assert_eq!(error.to_string(), "unexpected function call 'rm_rf'");
    // the name as Python's repr shows it: double quotes around a quote, None for a missing one, a number bare
    for (name, shown) in [(json!("it's"), "\"it's\""), (Value::Null, "None"), (json!(7), "7")] {
        let odd = json!({"output": [{"type": "function_call", "name": name, "call_id": "c9", "arguments": ""}]});
        assert_eq!(parse_response(&odd, &[], false).unwrap_err().to_string(), format!("unexpected function call {shown}"));
    }
    let (calls, _, _) = parse_response(&unknown, &[], true).unwrap();
    assert_eq!(calls[0]["unknown"], json!(true));
    assert_eq!(calls[0]["args"], json!({}));
    assert!(parse_response(&unknown, &["rm_rf".to_string()], false).is_ok()); // lent this turn
    let blank = json!({"output": [{"type": "function_call", "name": 7, "call_id": "c3"}]});
    assert_eq!(parse_response(&blank, &[], true).unwrap().0[0]["name"], json!(""));
    let bad = json!({"output": [{"type": "function_call", "name": "start_task", "call_id": "c", "arguments": "[1]"}]});
    assert_eq!(parse_response(&bad, &[], false).unwrap_err().to_string(), "function call arguments must be an object");
}

#[test]
fn allowed_names_take_what_owns_claims() {
    let owns: Arc<dyn Fn(&str) -> bool + Send + Sync> = Arc::new(|name: &str| name.starts_with("calendar__"));
    let allowed = AllowedNames::new(&["gmail__list".to_string()], Some(owns));
    assert!(allowed.contains("gmail__list") && allowed.contains("calendar__list_events") && !allowed.contains("x"));
    let call = json!({"output": [{"type": "function_call", "name": "calendar__list_events", "call_id": "c", "arguments": "{}"}]});
    assert!(parse_response_with(&call, &allowed, false).is_ok());
}

#[test]
fn empty_links_go_and_code_stays() {
    assert_eq!(strip_empty_links("See this ([]()) and [](https://a) or [label]()."), "See this and or label.");
    assert_eq!(strip_empty_links("code `[]()` stays"), "code `[]()` stays");
    assert_eq!(strip_empty_links("```\n[](x)\n``` ok [](y)"), "```\n[](x)\n``` ok");
}

#[test]
fn the_log_names_tools_and_slugs_never_arguments() {
    let calls = vec![
        obj(json!({"name": "COMPOSIO_MULTI_EXECUTE_TOOL", "args": {"tools": [{"tool_slug": "SLACK_SEND"}, {"x": 1}, "no"]}})),
        obj(json!({"name": "open_app", "args": {"apps": ["gmail", "calendar"], "app": "github"}})),
        obj(json!({"name": "open_app", "args": {}})),
        obj(json!({"name": "", "args": {"secret": "mail text"}})),
    ];
    assert_eq!(call_names(&calls), "COMPOSIO_MULTI_EXECUTE_TOOL[SLACK_SEND,?], open_app[gmail,calendar,github], open_app[?], ?");
    let tools = vec![obj(json!({"type": "web_search"})), obj(json!({"name": "remember"}))];
    assert_eq!(offered_names(&tools), vec!["remember".to_string()]);
}

#[test]
fn a_call_says_the_words_and_texts_the_links() {
    let (say, text) = split_for_call("It's at https://x.example/a. Bye\n📎\nThe table");
    assert_eq!(say, "It's at https://x.example/a. Bye");
    assert_eq!(text, "The table\nhttps://x.example/a");
    assert_eq!(split_for_call("plain"), ("plain".to_string(), String::new()));
}

#[test]
fn a_stream_says_each_sentence_and_never_the_written_part() {
    let heard = Arc::new(Mutex::new(Vec::<String>::new()));
    let h = heard.clone();
    let stream = SentenceStream::new(Arc::new(move |s: &str| h.lock().push(s.to_string())), 12);
    stream.feed("Hi. It is sunny ");
    stream.feed("today. Then rain");
    assert_eq!(*heard.lock(), vec!["Hi. It is sunny today.".to_string()]);
    stream.feed(" later 📎 a table");
    stream.feed(" more");
    stream.flush();
    assert_eq!(*heard.lock(), vec!["Hi. It is sunny today.".to_string(), "Then rain later".to_string()]);
    assert_eq!(stream.text(), "Hi. It is sunny today. Then rain later 📎 a table more");
}

// ---- the owner's files ------------------------------------------------------------------------------------------

#[test]
fn a_path_is_the_owners_home_and_never_a_hidden_folder() {
    let home = tmp();
    let h = home.path().canonicalize().unwrap();
    std::fs::write(h.join("a.txt"), "hi").unwrap();
    std::fs::create_dir(h.join(".ssh")).unwrap();
    std::fs::write(h.join(".ssh/id"), "secret").unwrap();
    assert_eq!(resolve_owner_path("~/a.txt", Some(&h)), (Some(h.join("a.txt")), String::new()));
    assert_eq!(resolve_owner_path("a.txt", Some(&h)).0, Some(h.join("a.txt")));
    assert_eq!(resolve_owner_path("~/.ssh/id", Some(&h)).1, "hidden folders and files are off limits");
    assert_eq!(resolve_owner_path("/etc/hosts", Some(&h)).1, "only files in the owner's home folder can be sent");
    assert_eq!(resolve_owner_path("~/nope.txt", Some(&h)).1, "no such file");
    assert_eq!(resolve_owner_path("  ", Some(&h)).1, "no path given");
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink("/etc", h.join("etc")).unwrap();
        assert_eq!(resolve_owner_path("~/etc/hosts", Some(&h)).1, "only files in the owner's home folder can be sent");
    }
}

#[test]
fn a_tasks_result_sends_the_files_it_made() {
    let home = tmp();
    let h = home.path().canonicalize().unwrap();
    std::fs::write(h.join("Eve Photo 1.jpg"), "jpeg").unwrap();
    std::fs::write(h.join("notes.md"), "notes").unwrap();
    // a bare path starts at ~/ or /Users/ only; a link or `code` may be anywhere
    let text = format!("Saved ~/Eve Photo 1.jpg. Also `{}/notes.md` and [x](file://{}/notes.md).", h.display(), h.display());
    let files = task_files(&text, wall_now() - 60.0, Some(&h));
    assert_eq!(files, vec![h.join("notes.md"), h.join("Eve Photo 1.jpg")]);
    assert!(task_files(&text, wall_now() + 3600.0, Some(&h)).is_empty()); // an old file is never sent
}

fn wall_now() -> f64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64()
}

#[test]
fn list_and_read_stay_in_the_home_folder() {
    let home = tmp();
    let h = home.path().canonicalize().unwrap();
    std::fs::create_dir(h.join("docs")).unwrap();
    std::fs::write(h.join("docs/a.md"), "# hello").unwrap();
    std::fs::write(h.join("docs/.hidden"), "x").unwrap();
    std::fs::write(h.join("docs/b.bin"), [0u8, 1, 2]).unwrap();
    std::fs::write(h.join("docs/c.txt"), [b'a', 0, b'b']).unwrap();
    let listing = list_files("~/docs", Some(&h));
    assert_eq!(listing["ok"], json!(true));
    let mut names: Vec<String> = listing["files"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap().to_string()).collect();
    names.sort();
    assert_eq!(names, ["a.md", "b.bin", "c.txt"]);
    assert_eq!(listing["more"], json!(0));
    assert_eq!(list_files("~/docs/a.md", Some(&h))["reason"], json!("that is a file, not a folder"));
    let read = read_file("~/docs/a.md", Some(&h));
    assert_eq!((read["ok"].clone(), read["text"].clone(), read["clipped"].clone()), (json!(true), json!("# hello"), json!(false)));
    assert_eq!(read_file("~/docs", Some(&h))["reason"], json!("that is a folder; list_files shows what it holds"));
    assert_eq!(read_file("~/docs/b.bin", Some(&h))["reason"], json!(".bin is not a plain-text file; send_file sends it here"));
    assert_eq!(read_file("~/docs/c.txt", Some(&h))["reason"], json!("that file is not text; send_file sends it here"));
}

// ---- the Bot API on the wire ------------------------------------------------------------------------------------

#[tokio::test]
async fn test_the_menu_request_is_scoped_to_the_owners_chat() {
    let _iso = isolated();
    let transport = MockTransport::ok(json!(true));
    let api = bot(&transport);
    let commands: Vec<(String, String)> = BOT_COMMANDS.iter().map(|(c, d)| (c.to_string(), d.to_string())).collect();
    api.set_commands(&commands, OWNER).await.unwrap();
    TelegramApi::typing(&api, OWNER).await.unwrap();
    TelegramApi::close(&api).await;
    let calls = transport.calls();
    let listed: Vec<Value> = BOT_COMMANDS.iter().map(|(c, d)| json!({"command": c, "description": d})).collect();
    assert_eq!(calls[0], ("setMyCommands".to_string(), json!({"commands": listed, "scope": {"type": "chat", "chat_id": OWNER}})));
    assert_eq!(calls[1], ("sendChatAction".to_string(), json!({"chat_id": OWNER, "action": "typing"})));
    assert!(transport.requests.lock()[0].url.starts_with(&format!("https://api.telegram.org/bot{TOKEN}/")));
}

#[tokio::test]
async fn test_the_draft_request_is_an_empty_placeholder_with_a_draft_id() {
    let _iso = isolated();
    let transport = MockTransport::ok(json!(true));
    let api = bot(&transport);
    api.send_draft(OWNER, 7, "").await.unwrap();
    api.send_draft(OWNER, 7, "partial").await.unwrap();
    let calls = transport.calls();
    // empty text is Telegram's "Thinking…" placeholder: no text field at all, and no Stop button (can_stop)
    assert_eq!(calls[0], ("sendMessageDraft".to_string(), json!({"chat_id": OWNER, "draft_id": 7})));
    assert_eq!(calls[1], ("sendMessageDraft".to_string(), json!({"chat_id": OWNER, "draft_id": 7, "text": "partial"})));
}

#[tokio::test]
async fn a_bot_api_refusal_carries_its_code_and_never_the_token() {
    let _iso = isolated();
    let transport = MockTransport::new(|_| {
        Ok(respond(400, &json!({"ok": false, "error_code": 400, "description": format!("bad bot{TOKEN} here")})))
    });
    let error = bot(&transport).get_me().await.unwrap_err();
    assert_eq!((error.code, error.description.as_str()), (400, "bad bot<token> here"));
    assert_eq!(error.to_string(), "400: bad bot<token> here");
    let down = MockTransport::new(|_| Err(HttpError { kind: "ConnectError".into(), message: format!("https://api.telegram.org/bot{TOKEN}") }));
    let error = bot(&down).get_me().await.unwrap_err();
    assert_eq!((error.code, error.to_string()), (0, "0: ConnectError".to_string()));
    let html = MockTransport::new(|_| Ok(HttpResponse { status: 502, headers: vec![], body: b"<html>".to_vec() }));
    assert_eq!(bot(&html).get_me().await.unwrap_err().to_string(), "502: reply was not JSON");
    let odd = MockTransport::new(|_| Ok(respond(200, &json!([1]))));
    assert_eq!(bot(&odd).get_me().await.unwrap_err().to_string(), "200: malformed reply");
    let silent = MockTransport::new(|_| Ok(respond(409, &json!({"ok": false}))));
    assert_eq!(bot(&silent).get_me().await.unwrap_err().to_string(), "409: no description");
}

#[tokio::test]
async fn a_message_telegram_will_not_parse_goes_again_as_plain_text() {
    let _iso = isolated();
    let transport = MockTransport::new(|request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        if body.get("parse_mode").is_some() {
            Ok(respond(400, &json!({"ok": false, "error_code": 400, "description": "Bad Request: can't parse entities"})))
        } else {
            Ok(respond(200, &json!({"ok": true, "result": {"message_id": 9}})))
        }
    });
    let api = bot(&transport);
    let id = api.send_inline(OWNER, "Go?", &[vec![("Go".into(), "k1".into(), "success".into()), ("No".into(), "k2".into(), "".into())]], None, None).await.unwrap();
    assert_eq!(id, 9);
    let calls = transport.calls();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[1].1["reply_markup"], json!({"inline_keyboard": [[{"text": "Go", "callback_data": "k1", "style": "success"},
                                                                        {"text": "No", "callback_data": "k2"}]]}));
    assert!(calls[1].1.get("parse_mode").is_none());
}

#[tokio::test]
async fn a_reply_and_a_reply_box_ride_the_first_and_last_pieces() {
    let _iso = isolated();
    let transport = MockTransport::ok(json!({"message_id": 1}));
    let api = bot(&transport);
    api.send_message(OWNER, "hello", None, None, &[], 55, Some("Your answer")).await.unwrap();
    api.send_message(OWNER, "pick", None, None, &["A".to_string(), "B".to_string()], 0, None).await.unwrap();
    let calls = transport.calls();
    assert_eq!(calls[0].1["reply_parameters"], json!({"message_id": 55, "allow_sending_without_reply": true}));
    assert_eq!(calls[0].1["reply_markup"], json!({"force_reply": true, "input_field_placeholder": "Your answer"}));
    assert_eq!(calls[0].1["parse_mode"], json!("HTML"));
    assert_eq!(calls[1].1["reply_markup"], json!({"keyboard": [[{"text": "A"}], [{"text": "B"}]], "one_time_keyboard": true, "resize_keyboard": true}));
}

#[tokio::test]
async fn an_edit_keeps_its_buttons_only_when_asked_and_the_rest_of_the_wire() {
    let _iso = isolated();
    let transport = MockTransport::ok(json!(true));
    let api = bot(&transport);
    api.edit_message(OWNER, 5, "done", None, None, None).await.unwrap();
    api.edit_message(OWNER, 5, "step", None, None, Some(&[vec![("Stop".into(), "k9".into(), "danger".into())]])).await.unwrap();
    api.drop_buttons(OWNER, 5).await.unwrap();
    api.answer_callback("q1", &"x".repeat(300)).await.unwrap();
    api.answer_callback("q2", "").await.unwrap();
    api.react(OWNER, 5, "👍").await.unwrap();
    api.react(OWNER, 5, "").await.unwrap();
    api.send_web_apps(OWNER, "apps", &[("Open".into(), "https://a".into())]).await.unwrap();
    api.send_url_buttons(OWNER, "links", &[("Read".into(), "https://b".into())]).await.unwrap();
    let updates = MockTransport::ok(json!([{"update_id": 1}, "junk"]));
    assert_eq!(bot(&updates).get_updates(Some(4), 50).await.unwrap(), vec![obj(json!({"update_id": 1}))]);
    let calls = transport.calls();
    assert_eq!(calls[0], ("editMessageText".to_string(), json!({"chat_id": OWNER, "message_id": 5, "reply_markup": {"inline_keyboard": []}, "text": "done", "parse_mode": "HTML"})));
    assert_eq!(calls[1].1["reply_markup"], json!({"inline_keyboard": [[{"text": "Stop", "callback_data": "k9", "style": "danger"}]]}));
    assert_eq!(calls[2], ("editMessageReplyMarkup".to_string(), json!({"chat_id": OWNER, "message_id": 5, "reply_markup": {"inline_keyboard": []}})));
    assert_eq!(calls[3].1["text"].as_str().unwrap().chars().count(), MAX_TOAST_CHARS);
    assert_eq!(calls[4].1, json!({"callback_query_id": "q2"}));
    assert_eq!(calls[5].1["reaction"], json!([{"type": "emoji", "emoji": "👍"}]));
    assert_eq!(calls[6].1["reaction"], json!([]));
    assert_eq!(calls[7].1["reply_markup"], json!({"inline_keyboard": [[{"text": "Open", "web_app": {"url": "https://a"}}]]}));
    assert_eq!(calls[8].1["reply_markup"], json!({"inline_keyboard": [[{"text": "Read", "url": "https://b"}]]}));
    assert_eq!(updates.calls()[0].1, json!({"timeout": 50, "allowed_updates": ["message", "callback_query"], "offset": 4}));
}

#[tokio::test]
async fn a_photo_goes_as_a_form_with_its_file() {
    let _iso = isolated();
    let dir = tmp();
    let picture = dir.path().join("shot.png");
    std::fs::write(&picture, b"PNGDATA").unwrap();
    let transport = MockTransport::ok(json!({"message_id": 3}));
    let api = bot(&transport);
    api.send_photo(OWNER, &picture, "here **it** is").await.unwrap();
    api.send_photo_web_apps(OWNER, b"JPEG", "🧮 Calculator", &[("Open".into(), "https://app".into())]).await.unwrap();
    let requests = transport.requests.lock().clone();
    let form = String::from_utf8_lossy(&requests[0].body).into_owned();
    assert!(requests[0].header("Content-Type").unwrap().starts_with("multipart/form-data; boundary="));
    assert!(requests[0].url.ends_with("/sendPhoto"));
    assert!(form.contains("name=\"chat_id\"\r\n\r\n42\r\n"));
    assert!(form.contains("name=\"photo\"; filename=\"shot.png\"\r\nContent-Type: image/png\r\n\r\nPNGDATA\r\n"));
    let form = String::from_utf8_lossy(&requests[1].body).into_owned();
    assert!(form.contains("name=\"reply_markup\"\r\n\r\n{\"inline_keyboard\": [[{\"text\": \"Open\", \"web_app\": {\"url\": \"https://app\"}}]]}\r\n"));
    assert!(form.contains("filename=\"app.jpg\"\r\nContent-Type: image/jpeg\r\n\r\nJPEG\r\n"));
    let missing = api.send_document(OWNER, &dir.path().join("gone.pdf"), "").await.unwrap_err();
    assert_eq!((missing.code, missing.description.as_str()), (0, "FileNotFoundError"));
    // a file name as httpx quotes it in the form: `"` as %22, a backslash doubled
    let odd = dir.path().join("odd \"name\"\\x.txt");
    std::fs::write(&odd, b"T").unwrap();
    api.send_document(OWNER, &odd, "").await.unwrap();
    let form = String::from_utf8_lossy(&transport.requests.lock()[2].body).into_owned();
    assert!(form.contains("name=\"document\"; filename=\"odd %22name%22\\\\x.txt\"\r\nContent-Type: application/octet-stream\r\n"), "{form}");
}

#[test]
fn test_app_only_still_keeps_the_token_out_of_logs() {
    let _iso = isolated();
    let _api = MutedBotApi::new(TOKEN, None);
    let line = redact(&format!("POST https://api.telegram.org/bot{TOKEN}/sendMessage"));
    assert!(!line.contains(TOKEN) && line.contains("bot<token>/sendMessage"));
    let mut out = Vec::new();
    {
        use std::io::Write;
        let mut writer = RedactingWriter(&mut out);
        writer.write_all(format!("GET bot{TOKEN}\n").as_bytes()).unwrap();
    }
    assert_eq!(String::from_utf8(out).unwrap(), "GET bot<token>\n");
    unhide_tokens();
    assert!(redact(TOKEN).contains(TOKEN)); // the control: nothing hidden, nothing changed
}

// ---- test_telegram_app_only.py ------------------------------------------------------------------------------------

#[test]
fn test_app_turns_on_the_brain_without_the_door() {
    let env = |pairs: &[(&str, &str)]| -> eve_brain::prelude::Environ {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    let cfg = configured(Some(&env(&[("EVE_TELEGRAM", "app"), ("EVE_TELEGRAM_TOKEN", TOKEN), ("EVE_TELEGRAM_OWNER", "42")])));
    assert!(cfg.enabled && cfg.app_only);
    let on = configured(Some(&env(&[("EVE_TELEGRAM", "1"), ("EVE_TELEGRAM_TOKEN", TOKEN), ("EVE_TELEGRAM_OWNER", "42")])));
    assert!(on.enabled && !on.app_only);
    // still needs an owner: the brain answers one person
    assert!(!configured(Some(&env(&[("EVE_TELEGRAM", "app"), ("EVE_TELEGRAM_TOKEN", TOKEN)]))).enabled);
    unhide_tokens();
}

#[tokio::test]
async fn test_the_muted_api_sends_nothing_and_every_send_looks_sent() {
    let _iso = isolated();
    let api = MutedBotApi::new("", None);
    assert!(api.muted());
    api.send_message(OWNER, "the receipt for c1", None, None, &[], 0, None).await.unwrap();
    let msg = api.send_inline(OWNER, "go?", &[vec![("Go".into(), "k1".into(), "".into())]], None, None).await.unwrap();
    api.typing(OWNER).await.unwrap();
    api.set_commands(&[("jobs".into(), "your jobs".into())], OWNER).await.unwrap();
    TelegramApi::close(&api).await;
    let updates = api.get_updates(None, 50).await.unwrap();
    let me = api.get_me().await.unwrap();
    assert_eq!(msg, 0);
    assert!(updates.is_empty());
    assert_eq!(me["username"], json!("muted"));
    assert!(api.dropped() >= 3);
    // no HTTP client: nothing can leave the Mac (MutedBotApi has none to reach with), and an image is refused
    let refused = api.receive_image(&eve_telegram_images::Attachment::new("f")).await;
    assert!(matches!(refused, Err(ApiImageError::Image(_))));
}

#[tokio::test]
async fn test_make_inlet_lends_the_muted_api_in_app_mode() {
    let _iso = isolated();
    let cfg = TelegramConfig { app_only: true, ..config() };
    let env: eve_brain::prelude::Environ = [("EVE_NOTIFY".to_string(), "0".to_string())].into_iter().collect();
    assert!(make_api(&cfg, Some(&env)).muted());
    assert!(!make_api(&config(), Some(&env)).muted());
    // make_inlet itself, with a model key in the environment it is lent: the inlet it builds holds the muted API
    let mut keyed = env.clone();
    keyed.insert("OPENROUTER_API_KEY".into(), "sk-test".into());
    let options = InletOptions {
        environ: Some(keyed),
        toolset_route: Lent::Given(None),
        grader: Lent::Given(None),
        small_talk_ask: Lent::Given(None),
        ..InletOptions::default()
    };
    let inlet = make_inlet(cfg.clone(), options).expect("an inlet in app mode");
    assert!(inlet.api.muted());
    // the control: the door shut without a model key, so no inlet is made at all
    let no_key = InletOptions { environ: Some(Default::default()), ..InletOptions::default() };
    assert!(make_inlet(cfg.clone(), no_key).is_none());
    assert!(make_inlet(TelegramConfig { enabled: false, ..cfg }, InletOptions::default()).is_none());
}

#[tokio::test]
async fn test_app_only_run_never_polls_and_shuts_down_cleanly() {
    let _iso = isolated();
    let cfg = TelegramConfig { app_only: true, ..config() };
    let fake = FakeApi::new(vec![]);
    let options = InletOptions {
        environ: Some(Default::default()),
        toolset_route: Lent::Given(None),
        grader: Lent::Given(None),
        small_talk_ask: Lent::Given(None),
        sleep: Some(no_sleep()),
        ..InletOptions::default()
    };
    let codex = Arc::new(ClosedCodex::default());
    let options = InletOptions { codex: Some(codex.clone()), ..options };
    let inlet = TelegramInlet::new(cfg, Arc::new(Wrapped(fake, Odd { never_poll: true, ..Odd::default() })), FakeCreate::new(vec![]).create(), options);
    let task = tokio::spawn(async move { inlet.run().await });
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert!(!task.is_finished());
    assert!(!codex.closed.load(std::sync::atomic::Ordering::SeqCst)); // the control: nothing shut down yet
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    // `shut == [True]`: the cancelled run still shut the inlet down (its last step closes the Codex door)
    for _ in 0..100 {
        if codex.closed.load(std::sync::atomic::Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(codex.closed.load(std::sync::atomic::Ordering::SeqCst), "the inlet was not shut down");
}

/// A Codex door that only remembers being closed: what `_shutdown` does last, so a closed door says it ran.
#[derive(Default)]
struct ClosedCodex {
    closed: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl eve_brain::prelude::CodexDoor for ClosedCodex {
    fn connected(&self) -> bool {
        false
    }
    fn running(&self) -> bool {
        false
    }
    async fn start(
        &self,
        _folder: &std::path::Path,
        _emit: eve_codex_chat::Emit,
        _picture: Option<eve_codex_chat::Picture>,
        _done: Option<eve_codex_chat::Emit>,
    ) -> Result<(), eve_codex_computer::CodexError> {
        Err(eve_codex_computer::CodexError::Unavailable("not in this test".into()))
    }
    async fn send(&self, _text: &str) -> Result<(), eve_codex_computer::CodexError> {
        Err(eve_codex_computer::CodexError::Unavailable("not in this test".into()))
    }
    async fn interrupt(&self) -> Result<(), eve_codex_computer::CodexError> {
        Ok(())
    }
    async fn close(&self) {
        self.closed.store(true, std::sync::atomic::Ordering::SeqCst);
    }
}

// ---- G10: the poll loop -----------------------------------------------------------------------------------------

#[tokio::test]
async fn test_an_update_is_handled_once() {
    let _iso = isolated();
    let api = FakeApi::new(vec![vec![said("one", 70), said("two", 71)], vec![], vec![said("three", 72)]]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![reply("1"), reply("2"), reply("3")]), config(), |_| {});
    run_rig(&rig, || async {}).await;
    assert_eq!(api.log.lock().offsets, vec![None, Some(72), Some(72), Some(73)]);
    assert_eq!(api.texts(), ["1", "2", "3"]);
    assert_eq!(rig.create.requests.lock().len(), 3);
}

#[tokio::test]
async fn test_a_network_error_backs_off_and_polling_resumes() {
    let _iso = isolated();
    let naps = Arc::new(Mutex::new(Vec::<f64>::new()));
    let api = FakeApi::new(vec![]);
    for e in [api_error(0, "ConnectError"), api_error(502, "Bad Gateway"), api_error(0, "ReadTimeout")] {
        api.feed_error(e);
    }
    api.feed(vec![said("still there?", 1)]);
    api.feed_error(api_error(0, "ConnectError"));
    let n = naps.clone();
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![reply("Yep!")]), config(), move |o| {
        o.sleep = Some(Arc::new(move |secs: f64| {
            n.lock().push(secs);
            async { tokio::task::yield_now().await }.boxed()
        }));
    });
    run_rig(&rig, || async {}).await;
    assert_eq!(*naps.lock(), vec![1.0, 2.0, 4.0, 1.0]); // doubles, and a good poll resets it
    assert_eq!(api.log.lock().sent, vec![(OWNER, "Yep!".to_string())]);
}

#[tokio::test]
async fn test_a_slow_turn_does_not_stop_the_next_poll() {
    let _iso = isolated();
    let hold = Arc::new(Flag::new());
    let api = FakeApi::new(vec![vec![said("think about this", 1)], vec![said("stop", 2)]]);
    let create = FakeCreate::new(vec![]);
    let h = hold.clone();
    create.push(Scripted::Later(Box::new(move || {
        async move {
            h.wait().await;
            Ok(reply("finally"))
        }
        .boxed()
    })));
    let rig = Rig::new(api.clone(), create, config(), |_| {});
    let inlet = rig.inlet.clone();
    let loop_task = tokio::spawn(async move { inlet.run().await });
    settle(200).await;
    assert_eq!(api.log.lock().polls, 3);
    assert_eq!(api.log.lock().sent, vec![(OWNER, NOTHING_TO_STOP_LINE.to_string())]); // "stop" answered mid-turn
    hold.set();
    settle(200).await;
    assert_eq!(api.log.lock().sent.last().cloned(), Some((OWNER, "finally".to_string())));
    loop_task.abort();
    let _ = loop_task.await;
}

/// The log lines written at ERROR while `f` runs (caplog.at_level(logging.ERROR)).
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

#[tokio::test]
async fn test_a_bad_token_or_a_second_poller_stops_the_inlet() {
    let _iso = isolated();
    for code in [401, 404, 409] {
        let api = FakeApi::new(vec![]);
        api.feed_error(api_error(code, "nope"));
        api.feed(vec![said("hello", 1)]);
        let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), |_| {});
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::ERROR)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        let done = tokio::time::timeout(std::time::Duration::from_secs(2), rig.inlet.run()).await;
        drop(guard);
        assert!(done.is_ok(), "the inlet kept polling after {code}");
        assert_eq!(api.log.lock().polls, 1);
        let text = String::from_utf8(captured.0.lock().clone()).unwrap();
        let errors: Vec<&str> = text.lines().filter(|l| l.contains("ERROR")).collect();
        assert_eq!(errors.len(), 1, "{text}");
        assert!(errors[0].contains(&format!("answered {code}")));
    }
}

// ---- the / menu (setMyCommands) ---------------------------------------------------------------------------------

fn bot_commands() -> Vec<(String, String)> {
    BOT_COMMANDS.iter().map(|(c, d)| (c.to_string(), d.to_string())).collect()
}

#[tokio::test]
async fn test_the_menu_is_set_once_for_each_owner_chat_and_only_there() {
    let _iso = isolated();
    let two = TelegramConfig { owner_ids: vec![OWNER, 77], ..config() };
    let api = FakeApi::new(vec![]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), two, |_| {});
    run_rig(&rig, || async {
        // sorted(owner_ids): one setMyCommands per owner chat
        assert_eq!(api.log.lock().commands, vec![(bot_commands(), OWNER), (bot_commands(), 77)]);
    })
    .await;
}

#[tokio::test]
async fn test_a_menu_telegram_refuses_leaves_eve_answering() {
    let _iso = isolated();
    let fake = FakeApi::new(vec![vec![said("stop", 1)]]);
    let create = FakeCreate::new(vec![]);
    let options = InletOptions {
        environ: Some(Default::default()),
        toolset_route: Lent::Given(None),
        grader: Lent::Given(None),
        small_talk_ask: Lent::Given(None),
        sleep: Some(no_sleep()),
        wall: Some(Arc::new(|| NOW)),
        brief: Some(Arc::new(String::new)),
        screen: Some(Arc::new(|| None)),
        ..InletOptions::default()
    };
    let inlet = TelegramInlet::new(config(), Arc::new(Wrapped(fake.clone(), Odd { no_menu: true, ..Odd::default() })), create.create(), options);
    let runner = inlet.clone();
    let loop_task = tokio::spawn(async move { runner.run().await });
    settle(200).await;
    assert_eq!(fake.log.lock().sent, vec![(OWNER, NOTHING_TO_STOP_LINE.to_string())]); // the poll loop lived on
    assert!(!loop_task.is_finished());
    loop_task.abort();
    let _ = loop_task.await;
}

#[test]
fn test_every_menu_command_is_well_formed() {
    let names: Vec<&str> = BOT_COMMANDS.iter().map(|(c, _)| *c).collect();
    let unique: std::collections::HashSet<&str> = names.iter().copied().collect();
    assert!(names.len() == unique.len() && unique.len() <= 100);
    let shape = regex::Regex::new(r"^[a-z0-9_]{1,32}$").unwrap();
    for (command, description) in BOT_COMMANDS {
        assert!(shape.is_match(command), "{command}");
        assert!((1..=256).contains(&description.chars().count()));
    }
}

/// test_telegram.py's MENU_CHECKS: the words typed before, and what the command must leave behind.
const MENU_COMMANDS: [&str; 14] =
    ["apps", "watch", "meet", "spend", "stick", "claude_on", "claude_off", "new_claude", "codex", "rundown", "screenshot", "stealth", "wake", "stop"];

#[test]
fn test_the_menu_offers_only_commands_the_dispatch_knows() {
    let mut checks: Vec<&str> = MENU_COMMANDS.to_vec();
    checks.sort();
    let mut offered: Vec<&str> = BOT_COMMANDS.iter().map(|(c, _)| *c).collect();
    offered.sort();
    assert_eq!(checks, offered);
}

const CODEX_FOLDER: &str = "/projects/eve";

/// `_menu_rig`: a terminal, one Claude session, a launcher, no recents, and one Codex folder.
fn menu_rig(api: Arc<FakeApi>, create: Arc<FakeCreate>) -> Rig {
    Rig::new(api, create, config(), |o| {
        o.terminal = Some(Arc::new(|_cwd: String, _text: String| async { Ok(String::new()) }.boxed()));
        o.claude_sessions = Some(Arc::new(|| Ok(Sessions::Now(vec!["/Users/g/repo".to_string()]))));
        o.launcher = Some(Arc::new(|_folder: PathBuf, _harness: String| async { Ok("opened".to_string()) }.boxed()));
        o.launch_recent = Some(Arc::new(Vec::new));
        o.codex_folders = Some(Arc::new(|| vec![PathBuf::from(CODEX_FOLDER)]));
    })
}

/// test_each_menu_command_is_accepted_as_the_menu_sends_it, one command: the words before it and the command
/// itself go through the poll loop, as the phone's menu sends them.
async fn check_menu_command(command: &str) {
    let _iso = isolated();
    let before: Vec<&str> = match command {
        "claude_off" => vec!["/claude_on"],
        "wake" => vec!["/stealth"],
        _ => vec![],
    };
    let api = FakeApi::new(vec![]);
    let create = FakeCreate::new(if command == "rundown" { vec![reply("Your day.")] } else { vec![] });
    let rig = menu_rig(api.clone(), create.clone());
    let typed: Vec<String> = before.iter().map(|s| s.to_string()).chain([format!("/{command}")]).collect();
    run_rig(&rig, || async {
        for (i, text) in typed.iter().enumerate() {
            api.feed(vec![said(text, i as i64 + 1)]);
            settle(200).await;
        }
        let last = api.log.lock().sent.last().cloned().unwrap_or_default();
        let inlet = &rig.inlet;
        match command {
            "apps" => assert_eq!(last, (OWNER, APPS_OFF_LINE.to_string())), // no Mini App in this rig: says so
            "watch" => assert_eq!(last, (OWNER, WATCH_OFF_LINE.to_string())), // no Watcher in this rig: says so, by code
            "meet" => assert_eq!(last, (OWNER, MEET_OFF_LINE.to_string())), // no Meeter in this rig: says so, by code
            "spend" => assert!(last.1.starts_with("Spending (eve's own meter)")), // answered by code: no model turn
            "stick" => assert_eq!(last, (OWNER, eve_stick_link::OFF_LINE.to_string())), // no Mini App: says so, by code
            "claude_on" => assert!(inlet.claude() && last == (OWNER, CLAUDE_ON_LINE.to_string())),
            "claude_off" => assert!(!inlet.claude() && last == (OWNER, CLAUDE_OFF_LINE.to_string())),
            "new_claude" => assert!(!api.log.lock().buttons.last().unwrap().is_empty()), // the tree's first question, as buttons
            "codex" => assert_eq!(last, (OWNER, "eve".to_string())), // the folder menu, not a model turn
            "rundown" => {
                // the rundown is a model turn with the daily skill in it, not an ordinary chat turn
                assert!(create.requests.lock().iter().any(|r| r.to_string().to_lowercase().contains("rundown")));
                assert_eq!(last, (OWNER, "Your day.".to_string()));
            }
            "screenshot" => assert!(last.1.starts_with("I couldn't grab the screen")),
            "stealth" => assert!(inlet.stealth() && last == (OWNER, STEALTH_ON_LINE.to_string())),
            "wake" => assert!(!inlet.stealth() && last == (OWNER, STEALTH_OFF_LINE.to_string())),
            "stop" => assert_eq!(last, (OWNER, NOTHING_TO_STOP_LINE.to_string())),
            other => panic!("no check for {other}"),
        }
        if command != "rundown" {
            assert!(create.requests.lock().is_empty(), "{command}: code words make no model call");
        }
    })
    .await;
}

#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_apps() {
    check_menu_command("apps").await;
}
#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_watch() {
    check_menu_command("watch").await;
}
#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_meet() {
    check_menu_command("meet").await;
}
#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_spend() {
    check_menu_command("spend").await;
}
#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_stick() {
    check_menu_command("stick").await;
}
#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_claude_on() {
    check_menu_command("claude_on").await;
}
#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_claude_off() {
    check_menu_command("claude_off").await;
}
#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_new_claude() {
    check_menu_command("new_claude").await;
}
#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_codex() {
    check_menu_command("codex").await;
}
#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_rundown() {
    check_menu_command("rundown").await;
}
#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_screenshot() {
    check_menu_command("screenshot").await;
}
#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_stealth() {
    check_menu_command("stealth").await;
}
#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_wake() {
    check_menu_command("wake").await;
}
#[tokio::test]
async fn test_each_menu_command_is_accepted_as_the_menu_sends_it_stop() {
    check_menu_command("stop").await;
}

#[tokio::test]
async fn test_spend_is_answered_by_code_even_while_the_claude_relay_is_on() {
    let iso = isolated();
    let ledger = eve_spend::Ledger::at(iso.ledger.path());
    ledger.record("openai", "gpt-6-luna", "chat", Some(0.30), eve_spend::RecordOpts::new());
    ledger.record("openai", "gpt-live-1", "voice", Some(1.20), eve_spend::RecordOpts::new());
    let api = FakeApi::new(vec![]);
    let create = FakeCreate::new(vec![]);
    let rig = menu_rig(api.clone(), create.clone());
    run_rig(&rig, || async {
        api.feed(vec![said("/claude_on", 1)]);
        settle(200).await;
        api.feed(vec![said("spend", 2)]);
        settle(200).await;
        let text = api.log.lock().sent.last().cloned().unwrap().1;
        assert!(text.starts_with("Spending (eve's own meter)") && text.contains("Today: $1.50"), "{text}");
        assert!(text.contains("Top today: voice $1.20, chat $0.30"), "{text}");
        assert!(create.requests.lock().is_empty()); // no model call, not typed to Claude
    })
    .await;
}

#[tokio::test]
async fn test_each_chat_model_call_is_metered() {
    let iso = isolated();
    let mut answer = reply("Hi!");
    answer["usage"] = json!({"input_tokens": 1_000_000, "input_tokens_details": {"cached_tokens": 0}, "output_tokens": 0});
    let api = FakeApi::new(vec![vec![said("hello", 1)]]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![answer]), config(), |_| {});
    run_rig(&rig, || async {
        assert_eq!(api.log.lock().sent.last().cloned(), Some((OWNER, "Hi!".to_string())));
    })
    .await;
    let today = chrono::Local::now().date_naive().format("%Y-%m-%d").to_string();
    let rows = eve_spend::day_rows(&today, Some(iso.ledger.path()));
    assert_eq!(rows.len(), 1);
    let model = rig.create.requests.lock()[0]["model"].clone();
    assert_eq!((rows[0]["f"].clone(), rows[0]["m"].clone()), (json!("chat"), model));
    assert!((rows[0]["usd"].as_f64().unwrap() - 0.10).abs() < 1e-9); // gpt-6-luna: $0.10 per million in
}

#[tokio::test]
async fn test_the_underscore_words_also_take_a_target() {
    let _iso = isolated();
    let api = FakeApi::new(vec![]);
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![]), config(), |o| {
        o.terminal = Some(Arc::new(|_cwd: String, _text: String| async { Ok(String::new()) }.boxed()));
        o.claude_sessions =
            Some(Arc::new(|| Ok(Sessions::Now(vec!["/Users/g/repo".to_string(), "/Users/g/other".to_string()]))));
    });
    run_rig(&rig, || async {
        api.feed(vec![said("/claude_on other", 1)]);
        settle(200).await;
        assert!(rig.inlet.claude());
        assert_eq!(api.log.lock().sent.last().cloned().map(|s| s.1), Some(CLAUDE_ON_LINE.to_string()));
    })
    .await;
}

// ---- "Thinking…" drafts: the switch ---------------------------------------------------------------------------------

#[test]
fn test_the_drafts_ship_on_and_the_switch_turns_them_off() {
    let env = |pairs: &[(&str, &str)]| -> eve_brain::prelude::Environ {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    };
    assert!(DRAFTS_DEFAULT && TelegramConfig::default().drafts);
    assert!(configured(Some(&env(&[]))).drafts);
    assert!(!configured(Some(&env(&[("EVE_TELEGRAM_DRAFTS", "0")]))).drafts);
    assert!(!configured(Some(&env(&[("EVE_TELEGRAM_DRAFTS", "off")]))).drafts);
    assert!(DRAFT_REFRESH_SECS < 30.0); // a draft is a 30-second preview
}

// ---- "typing…" and "Thinking…": what reaches the phone ---------------------------------------------------------
//
// test_telegram.py also reads the inlet's private typing reasons and draft task; from outside, these check what
// the phone is shown (FakeApi's log), which is what those reasons are for.

#[tokio::test]
async fn test_a_eve_turn_keeps_typing_until_the_reply_and_no_longer() {
    let _iso = isolated();
    let release = Arc::new(Flag::new());
    let api = FakeApi::new(vec![vec![said("what's up", 1)]]);
    let create = FakeCreate::new(vec![]);
    let r = release.clone();
    create.push(Scripted::Later(Box::new(move || {
        async move {
            r.wait().await;
            Ok(reply("Here you go."))
        }
        .boxed()
    })));
    let rig = Rig::new(api.clone(), create, config(), |_| {});
    run_rig(&rig, || async {
        settle(400).await;
        // repeated while the model works, never more than the turn's cap in ticks
        let cap = (TYPING_TURN_SECS / TYPING_EVERY_SECS) as usize;
        {
            let log = api.log.lock();
            assert!(1 < log.typings.len() && log.typings.len() <= cap, "{}", log.typings.len());
            assert!(log.typings.iter().all(|c| *c == OWNER));
        }
        release.set();
        settle(200).await;
        assert_eq!(api.log.lock().sent.last().cloned(), Some((OWNER, "Here you go.".to_string())));
        assert_eq!(api.log.lock().order.last().map(String::as_str), Some("send")); // no "typing…" after the reply
        let after = api.log.lock().typings.len();
        settle(200).await;
        assert_eq!(api.log.lock().typings.len(), after);
    })
    .await;
}

#[tokio::test]
async fn test_a_failed_typing_is_tried_once_and_the_turn_still_answers() {
    let _iso = isolated();
    let fake = FakeApi::new(vec![vec![said("hi", 1)]]);
    let create = FakeCreate::new(vec![]);
    create.push(Scripted::Later(Box::new(|| {
        async {
            settle(20).await; // long enough for a few ticks
            Ok(reply("Hello!"))
        }
        .boxed()
    })));
    let options = InletOptions {
        environ: Some(Default::default()),
        toolset_route: Lent::Given(None),
        grader: Lent::Given(None),
        small_talk_ask: Lent::Given(None),
        sleep: Some(no_sleep()),
        wall: Some(Arc::new(|| NOW)),
        brief: Some(Arc::new(String::new)),
        screen: Some(Arc::new(|| None)),
        ..InletOptions::default()
    };
    let api = Arc::new(Wrapped(fake.clone(), Odd { no_typing: true, ..Odd::default() }));
    let inlet = TelegramInlet::new(config(), api, create.create(), options);
    let runner = inlet.clone();
    let loop_task = tokio::spawn(async move { runner.run().await });
    settle(400).await;
    assert_eq!(fake.log.lock().typings, vec![OWNER]);
    assert_eq!(fake.log.lock().sent, vec![(OWNER, "Hello!".to_string())]);
    loop_task.abort();
    let _ = loop_task.await;
}

/// `_relay_rig`: the relay joined to one session. The fake sleep is instant, so a draft's 20 s refresh wait is held
/// open (a turn stays "Thinking…" while the test looks at it).
fn relay_rig(api: Arc<FakeApi>, config: TelegramConfig) -> Rig {
    Rig::new(api, FakeCreate::new(vec![]), config, |o| {
        o.terminal = Some(Arc::new(|_cwd: String, _text: String| async { Ok(String::new()) }.boxed()));
        o.claude_sessions = Some(Arc::new(|| Ok(Sessions::Now(vec!["/Users/g/repo".to_string()]))));
        o.sleep = Some(Arc::new(|secs: f64| {
            async move {
                if secs == DRAFT_REFRESH_SECS {
                    futures::future::pending::<()>().await; // until the loop is cancelled
                }
                tokio::task::yield_now().await
            }
            .boxed()
        }));
    })
}

const REPO: &str = "/Users/g/repo";

#[tokio::test]
async fn test_a_relayed_line_types_until_claude_answers_asks_or_ends_its_turn() {
    let _iso = isolated();
    let api = FakeApi::new(vec![vec![said("claude on", 1)]]);
    let rig = relay_rig(api.clone(), TelegramConfig { drafts: false, ..config() });
    run_rig(&rig, || async {
        let inlet = &rig.inlet;
        assert!(inlet.claude() && api.log.lock().typings.is_empty());
        api.feed(vec![said("run the tests", 2)]);
        settle(20).await;
        assert!(!api.log.lock().typings.is_empty() && api.log.lock().drafts.is_empty());
        inlet.relay_text("All green.", REPO).await; // Claude's next words
        settle(20).await;
        let after = api.log.lock().typings.len();
        settle(50).await;
        assert_eq!(api.log.lock().typings.len(), after); // the answer ended it
        api.feed(vec![said("claude off", 3)]);
        settle(20).await;
        assert!(!inlet.claude());
    })
    .await;
}

#[tokio::test]
async fn test_a_relayed_turn_shows_thinking_between_claudes_messages_until_it_ends() {
    let _iso = isolated();
    let api = FakeApi::new(vec![vec![said("claude on", 1)]]);
    let rig = relay_rig(api.clone(), config());
    run_rig(&rig, || async {
        let inlet = &rig.inlet;
        assert!(inlet.claude() && api.log.lock().drafts.is_empty());
        api.feed(vec![said("run the tests", 2)]);
        settle(20).await;
        // the bubble, not "typing…", and never Claude's words in a draft
        {
            let log = api.log.lock();
            assert!(!log.drafts.is_empty() && log.drafts.iter().all(|d| *d == (OWNER, 1, String::new())));
            assert!(log.typings.is_empty());
        }
        inlet.relay_text("Running them now.", REPO).await; // Claude says a line, keeps working
        settle(50).await;
        {
            let log = api.log.lock();
            assert_eq!(log.sent.last().cloned(), Some((OWNER, "Running them now.".to_string())));
            assert_eq!(log.order[log.order.len() - 2..], ["send".to_string(), "draft".to_string()]); // shown again
        }
        inlet.relay_turn_ended(REPO); // the Stop hook
        let before = api.log.lock().drafts.len();
        inlet.relay_text("All green.", REPO).await; // the last words, flushed after the end
        settle(50).await;
        {
            let log = api.log.lock();
            assert_eq!(log.sent.last().cloned(), Some((OWNER, "All green.".to_string())));
            assert_eq!(log.drafts.len(), before);
            assert_eq!(log.order.last().map(String::as_str), Some("send")); // no bubble after the turn
        }
        // the next line is a new turn with a new draft id
        api.feed(vec![said("now ship it", 3)]);
        settle(20).await;
        assert_eq!(api.log.lock().drafts.last().cloned(), Some((OWNER, 2, String::new())));
        api.feed(vec![said("claude off", 4)]);
        settle(20).await;
        assert!(api.log.lock().typings.is_empty());
    })
    .await;
}

#[tokio::test]
async fn test_a_refused_draft_turns_drafts_off_and_typing_takes_over_with_nothing_lost() {
    let _iso = isolated();
    let api = FakeApi::new(vec![vec![said("claude on", 1)]]);
    *api.fail_drafts.lock() = Some(api_error(400, "Bad Request: method not found"));
    let rig = relay_rig(api.clone(), config());
    run_rig(&rig, || async {
        api.feed(vec![said("run the tests", 2)]);
        settle(20).await;
        assert!(api.log.lock().drafts.is_empty() && !api.log.lock().typings.is_empty()); // "typing…" for the time left
        rig.inlet.relay_text("All green.", REPO).await;
        settle(50).await;
        assert_eq!(api.log.lock().sent.last().cloned(), Some((OWNER, "All green.".to_string()))); // the words arrive
        *api.fail_drafts.lock() = None; // off for the process: not tried again
        api.feed(vec![said("next", 3)]);
        settle(20).await;
        assert!(api.log.lock().drafts.is_empty());
        rig.inlet.relay_turn_ended(REPO);
    })
    .await;
}

#[tokio::test]
async fn test_a_draft_refused_after_a_message_still_leaves_the_message_sent() {
    let _iso = isolated();
    let api = FakeApi::new(vec![vec![said("claude on", 1)]]);
    let rig = relay_rig(api.clone(), config());
    run_rig(&rig, || async {
        api.feed(vec![said("run the tests", 2)]);
        settle(20).await;
        assert!(!api.log.lock().drafts.is_empty());
        *api.fail_drafts.lock() = Some(api_error(429, "Too Many Requests: retry after 3"));
        rig.inlet.relay_text("Halfway there.", REPO).await;
        settle(50).await;
        assert_eq!(api.log.lock().sent.last().cloned(), Some((OWNER, "Halfway there.".to_string())));
        assert!(!api.log.lock().typings.is_empty()); // the turn still shows work
    })
    .await;
}

// ---- telegram-check ---------------------------------------------------------------------------------------------

#[tokio::test]
async fn diagnose_says_whether_the_token_is_good_and_never_polls_with_an_offset() {
    let _iso = isolated();
    let env: eve_brain::prelude::Environ = Default::default();
    assert_eq!(diagnose(Some(&env), None).await, 1); // no token
    let env: eve_brain::prelude::Environ =
        [("EVE_TELEGRAM_TOKEN".to_string(), TOKEN.to_string()), ("EVE_TELEGRAM_OWNER".to_string(), "42".to_string())].into_iter().collect();
    let api = FakeApi::new(vec![vec![said("hi", 1)]]);
    assert_eq!(diagnose(Some(&env), Some(api.clone())).await, 0);
    assert_eq!(api.log.lock().offsets, vec![None]);
    let refusing = FakeApi::new(vec![]);
    refusing.feed_error(api_error(409, "Conflict"));
    assert_eq!(diagnose(Some(&env), Some(refusing)).await, 1);
    assert_eq!(api.log.lock().polls, 1);
}

// ---- test_telegram.py G5 privacy and the real request path (483-628), as Python wrote them ----------------------

/// Python's RuntimeError is an `anyhow` message here, whose kind the turn's log line names "Exception"; httpx's own
/// "HTTP Request" line has no Rust counterpart (reqwest logs nothing), so the token's redaction is checked on the
/// lines eve writes.
#[tokio::test]
async fn test_words_and_the_token_never_reach_the_log() {
    let _iso = isolated();
    let logs = Caplog::capture();
    let secret = "my-bank-password-is-hunter2";
    let polls = Arc::new(Mutex::new(0));
    let p = polls.clone();
    let transport = MockTransport::new(move |request| {
        assert!(request.url.contains(TOKEN)); // the token really is in the URL: that is the danger
        let method = request.url.rsplit('/').next().unwrap_or("").to_string();
        if method == "getUpdates" {
            let mut n = p.lock();
            *n += 1;
            return match *n {
                1 => Ok(respond(200, &json!({"ok": true, "result": [
                    said(secret, 1), update(2, &format!("psst {secret}"), 666, NOW, 2)]}))),
                2 => Err(HttpError { kind: "ConnectError".into(), message: format!("cannot reach {}", request.url) }),
                3 => Ok(respond(500, &json!({"ok": false, "error_code": 500, "description": format!("failed for bot{TOKEN}")}))),
                _ => Ok(respond(409, &json!({"ok": false, "error_code": 409, "description": "Conflict"}))),
            };
        }
        Ok(respond(200, &json!({"ok": true, "result": {}})))
    });
    let api: Arc<dyn TelegramApi> = Arc::new(bot(&transport));
    let create = FakeCreate::new(vec![]);
    create.push(Scripted::Fail(format!("the model choked on: {secret}")));
    let cfg = TelegramConfig { token: TOKEN.into(), ..config() };
    let inlet = TelegramInlet::new(cfg, api, create.create(), InletOptions {
        wall: Some(Arc::new(|| NOW)),
        sleep: Some(no_sleep()),
        environ: Some(conftest_env()),
        toolset_route: Lent::Given(None),
        grader: Lent::Given(None),
        small_talk_ask: Lent::Given(None),
        ..InletOptions::default()
    });
    tokio::time::timeout(std::time::Duration::from_secs(5), inlet.run()).await.expect("ends by itself on the 409");
    settle(200).await;
    assert_eq!(create.requests.lock().len(), 1);
    let text = logs.text();
    assert!(text.contains("answered 409"), "{text}");
    // The controls: the capture is live and the drop line is there.
    assert!(text.contains("user id 666"));
    assert!(text.contains("turn failed: Exception") && text.contains("ConnectError"), "{text}");
    // The claims.
    assert!(!text.contains(TOKEN));
    assert!(!text.contains(secret) && !text.contains("hunter2"));
}

#[tokio::test]
async fn test_a_bot_api_error_never_carries_the_url() {
    let _iso = isolated();
    let transport = MockTransport::new(|request| {
        Err(HttpError { kind: "ReadTimeout".into(), message: format!("timed out: {}", request.url) })
    });
    let api = bot(&transport);
    let caught = api.get_updates(None, 50).await.unwrap_err();
    api.close().await;
    // Python's `__cause__ is None` and `__suppress_context__`: a BotApiError carries no source error at all
    assert!(!caught.to_string().contains(TOKEN) && std::error::Error::source(&caught).is_none());
    assert!(caught.to_string().contains("ReadTimeout"));
}

#[tokio::test]
async fn test_a_long_answer_is_sent_as_html_pieces_under_the_limit_and_nothing_is_lost() {
    let _iso = isolated();
    let paragraphs: Vec<String> = (0..20).map(|i| format!("Paragraph {i}: {}**end**", "word ".repeat(120))).collect();
    let answer = paragraphs.join("\n\n");
    let transport = MockTransport::ok(json!({}));
    let api = bot(&transport);
    api.send_message(OWNER, &answer, Some("Task done"), Some("the goal"), &[], 0, None).await.unwrap();
    api.close().await;
    let bodies: Vec<Value> = transport.calls().into_iter().map(|(_, b)| b).collect();
    assert!(bodies.len() >= 3 && bodies.iter().all(|b| b["parse_mode"] == json!("HTML") && b["chat_id"] == json!(OWNER)));
    assert!(bodies.iter().all(|b| b["text"].as_str().unwrap().chars().count() <= MAX_MESSAGE_CHARS));
    assert!(bodies[0]["text"].as_str().unwrap().starts_with("<b>Task done</b>\n<i>the goal</i>\n\nParagraph 0: "));
    for b in &bodies {
        // cut on a paragraph: a piece starts at one
        let t = b["text"].as_str().unwrap();
        assert!((t.starts_with("<b>Task done</b>") || t.starts_with("Paragraph ")) && t.ends_with("<b>end</b>"));
    }
    let shown = bodies.iter().map(|b| eve_telegram_format::visible(b["text"].as_str().unwrap())).collect::<Vec<_>>().join("\n\n");
    let want = paragraphs.iter().map(|p| p.replace("**", "").trim_end().to_string()).collect::<Vec<_>>().join("\n\n");
    assert_eq!(shown, format!("Task done\nthe goal\n\n{want}"));
}

#[tokio::test]
async fn test_a_message_telegram_cannot_parse_is_sent_again_as_plain_text() {
    let _iso = isolated();
    let transport = MockTransport::new(|request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap();
        let text = body["text"].as_str().unwrap_or("");
        if body.get("parse_mode") == Some(&json!("HTML")) && text.contains("odd") {
            return Ok(respond(400, &json!({"ok": false, "error_code": 400,
                                           "description": "Bad Request: can't parse entities: Unsupported start tag"})));
        }
        if text.contains("forbidden") {
            return Ok(respond(403, &json!({"ok": false, "error_code": 403, "description": "Forbidden: bot was blocked"})));
        }
        Ok(respond(200, &json!({"ok": true, "result": {}})))
    });
    let api = bot(&transport);
    api.send_message(OWNER, "an **odd** one <tag>", Some("Claude"), None, &[], 0, None).await.unwrap();
    let caught = api.send_message(OWNER, "forbidden", None, None, &[], 0, None).await.unwrap_err();
    assert_eq!(caught.code, 403);
    let bodies: Vec<Value> = transport.calls().into_iter().map(|(_, b)| b).collect();
    let modes: Vec<Option<&str>> = bodies.iter().map(|b| b.get("parse_mode").and_then(Value::as_str)).collect();
    assert_eq!(modes, vec![Some("HTML"), None, Some("HTML")]);
    assert_eq!(bodies[0]["text"], json!("<b>Claude</b>\n\nan <b>odd</b> one &lt;tag&gt;"));
    assert_eq!(bodies[1]["text"], json!("Claude\n\nan odd one <tag>"));
}

#[test]
fn test_english_calls_drop_words_in_another_script() {
    for text in ["あ。", "ご視聴ありがとうございました", "Продолжение следует", "谢谢"] {
        assert!(eve_phone_call::foreign(text, "en"), "{text}");
    }
}

#[test]
fn test_english_and_punctuation_are_kept() {
    for text in ["Who is the president?", "Play Beyoncé", "café at 5", "ok", "...", "42", ""] {
        assert!(!eve_phone_call::foreign(text, "en"), "{text}");
    }
}

#[test]
fn test_no_language_or_another_language_keeps_everything() {
    assert!(!eve_phone_call::foreign("あ。", ""));
    assert!(!eve_phone_call::foreign("あ。", "ja"));
}

#[tokio::test]
async fn test_the_mini_apps_pin_never_reaches_telegram_in_app_mode() {
    let _iso = isolated();
    let environ: eve_brain::prelude::Environ = [("EVE_TELEGRAM".to_string(), "app".to_string())].into_iter().collect();
    let bot = eve_brain::miniapp::bot_caller(TOKEN, Some(&environ));
    let reply = bot("setChatMenuButton".to_string(), json!({"chat_id": OWNER}).as_object().cloned().unwrap()).await.unwrap();
    unhide_tokens();
    assert!(reply["ok"] == json!(false) && reply["description"].as_str().unwrap().contains("EVE_TELEGRAM=app"), "{reply}");
}
