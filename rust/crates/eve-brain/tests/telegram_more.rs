//! test_telegram.py's tests the unit files did not carry: the screen and the task browser's picture (860-990), files
//! (991-1056), notes on the phone (1111-1133) and no emoji (1430-1455), ported with their names and assertions.
//!
//! The private attributes Python's tests set (`rig.inlet._agent = ...`, `rig.inlet._phone_notes = ...`) and the
//! private methods they call (`_run_agent`, `_send_screen`, `_robot_tool`) are the inlet's doc-hidden `tt_*` hooks
//! and its `phone_notes`. `monkeypatch.setattr(Path, "home", ...)` is a HOME in the environment lent to the inlet,
//! and `monkeypatch.setattr(telegram, "MAX_DOCUMENT_BYTES", 0)` is a sparse file one byte past the real limit.

mod support;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Value};

use eve_brain::prelude::{Http, HttpError, HttpRequest, HttpResponse, HttpTransport, PhoneNotes, RoomNotes, TaskAgent};
use eve_brain::telegram::{self as telegram, ApiImageError, BotApi, BotApiError, InletOptions, TelegramApi, TelegramInlet, MAX_DOCUMENT_BYTES};
use support::*;

const TOKEN: &str = "123456:AAsecretTOKENvalue";

// ---- what every test here shares ------------------------------------------------------------------------------

/// conftest.py's `_spend_ledger_in_tmp`: the ledger is process-wide, so the tests that run an inlet take this lock
/// and each gets a ledger folder of its own.
static SERIAL: Mutex<()> = Mutex::new(());

struct Isolated {
    _lock: parking_lot::MutexGuard<'static, ()>,
    _ledger: tempfile::TempDir,
}

impl Drop for Isolated {
    fn drop(&mut self) {
        eve_spend::set_dir(None);
    }
}

fn isolated() -> Isolated {
    let lock = SERIAL.lock();
    let ledger = tmp();
    eve_spend::set_dir(Some(ledger.path().to_path_buf()));
    Isolated { _lock: lock, _ledger: ledger }
}

fn obj(v: Value) -> Obj {
    v.as_object().cloned().expect("an object")
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

async fn run_plain(rig: &Rig) {
    run_rig(rig, || async {}).await;
}

/// `Rig(Api(...), create, **kw)` over an API that is not a FakeApi: the same lent fakes as `Rig::new`, `tweak` sets
/// the rest of the keyword arguments, and `rig.api` is an unused FakeApi.
fn rig_over(other: Arc<dyn TelegramApi>, create: Arc<FakeCreate>, tweak: impl FnOnce(&mut InletOptions)) -> Rig {
    let made = Rig::new(FakeApi::new(vec![]), create.clone(), config(), |_| {});
    let s = made.states.clone();
    let mut options = InletOptions {
        agent_factory: Some(made.agents.factory()),
        brief: Some(Arc::new(String::new)),
        on_state: Some(Arc::new(move |state: &str| s.lock().push(state.to_string()))),
        clock: Some(made.clock.clock()),
        wall: Some(Arc::new(|| NOW)),
        sleep: Some(no_sleep()),
        screen: Some(Arc::new(|| None)),
        toolset_route: eve_brain::prelude::Lent::Given(None),
        grader: eve_brain::prelude::Lent::Given(None),
        small_talk_ask: eve_brain::prelude::Lent::Given(None),
        environ: Some(conftest_env()),
        ..InletOptions::default()
    };
    tweak(&mut options);
    let inlet = TelegramInlet::new(config(), other, create.create(), options);
    Rig { inlet, ..made }
}

/// `json.loads(rig.create.requests[n]["input"][-1]["output"])`.
fn output_of(rig: &Rig, n: usize) -> Value {
    let requests = rig.create.requests.lock();
    let items = requests[n]["input"].as_array().expect("input");
    serde_json::from_str(items.last().unwrap()["output"].as_str().expect("an output")).unwrap()
}

// ---- a FakeApi with send_photo or send_document of its own (Python's `class Api(FakeApi)`) ---------------------

type FileHook = Box<dyn Fn(i64, &Path, &str) -> Result<(), BotApiError> + Send + Sync>;

struct Hooked {
    inner: Arc<FakeApi>,
    photo: Option<FileHook>,
    document: Option<FileHook>,
}

#[async_trait]
impl TelegramApi for Hooked {
    async fn get_me(&self) -> Result<Obj, BotApiError> {
        self.inner.get_me().await
    }
    async fn get_updates(&self, offset: Option<i64>, timeout: i64) -> Result<Vec<Obj>, BotApiError> {
        self.inner.get_updates(offset, timeout).await
    }
    async fn receive_image(&self, item: &eve_telegram_images::Attachment) -> Result<eve_telegram_images::ReceivedImage, ApiImageError> {
        self.inner.receive_image(item).await
    }
    async fn send_message(&self, chat_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, buttons: &[String], reply_to: i64, force_reply: Option<&str>) -> Result<(), BotApiError> {
        self.inner.send_message(chat_id, text, title, subtitle, buttons, reply_to, force_reply).await
    }
    async fn send_inline(&self, chat_id: i64, text: &str, keyboard: &[Vec<(String, String, String)>], title: Option<&str>, subtitle: Option<&str>) -> Result<i64, BotApiError> {
        self.inner.send_inline(chat_id, text, keyboard, title, subtitle).await
    }
    async fn edit_message(&self, chat_id: i64, message_id: i64, text: &str, title: Option<&str>, subtitle: Option<&str>, keyboard: Option<&[Vec<(String, String, String)>]>) -> Result<(), BotApiError> {
        self.inner.edit_message(chat_id, message_id, text, title, subtitle, keyboard).await
    }
    async fn drop_buttons(&self, chat_id: i64, message_id: i64) -> Result<(), BotApiError> {
        self.inner.drop_buttons(chat_id, message_id).await
    }
    async fn answer_callback(&self, query_id: &str, text: &str) -> Result<(), BotApiError> {
        self.inner.answer_callback(query_id, text).await
    }
    async fn react(&self, chat_id: i64, message_id: i64, emoji: &str) -> Result<(), BotApiError> {
        self.inner.react(chat_id, message_id, emoji).await
    }
    async fn send_photo(&self, chat_id: i64, path: &Path, caption: &str) -> Result<(), BotApiError> {
        match &self.photo {
            Some(hook) => hook(chat_id, path, caption),
            None => self.inner.send_photo(chat_id, path, caption).await,
        }
    }
    async fn send_document(&self, chat_id: i64, path: &Path, caption: &str) -> Result<(), BotApiError> {
        match &self.document {
            Some(hook) => hook(chat_id, path, caption),
            None => self.inner.send_document(chat_id, path, caption).await,
        }
    }
    async fn typing(&self, chat_id: i64) -> Result<(), BotApiError> {
        self.inner.typing(chat_id).await
    }
    async fn send_draft(&self, chat_id: i64, draft_id: i64, text: &str) -> Result<(), BotApiError> {
        self.inner.send_draft(chat_id, draft_id, text).await
    }
    async fn send_web_apps(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.inner.send_web_apps(chat_id, text, buttons).await
    }
    async fn send_url_buttons(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.inner.send_url_buttons(chat_id, text, buttons).await
    }
    async fn send_photo_web_apps(&self, chat_id: i64, jpeg: &[u8], caption: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.inner.send_photo_web_apps(chat_id, jpeg, caption, buttons).await
    }
    async fn set_commands(&self, commands: &[(String, String)], chat_id: i64) -> Result<(), BotApiError> {
        self.inner.set_commands(commands, chat_id).await
    }
}

// ---- an httpx.MockTransport ----------------------------------------------------------------------------------

/// Every request the client made, each answered `{"ok": true, "result": {}}`.
struct MockTransport {
    requests: Mutex<Vec<HttpRequest>>,
}

impl MockTransport {
    fn ok() -> Arc<Self> {
        Arc::new(Self { requests: Mutex::new(Vec::new()) })
    }
}

#[async_trait]
impl HttpTransport for MockTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, HttpError> {
        self.requests.lock().push(request);
        Ok(HttpResponse {
            status: 200,
            headers: vec![("content-type".into(), "application/json".into())],
            body: br#"{"ok": true, "result": {}}"#.to_vec(),
        })
    }
}

fn bot(transport: &Arc<MockTransport>) -> BotApi {
    let http: Http = transport.clone();
    BotApi::with_http(TOKEN, http)
}

fn content_type(request: &HttpRequest) -> String {
    request.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-type")).map(|(_, v)| v.clone()).unwrap_or_default()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

// ---- the task's own pictures ---------------------------------------------------------------------------------

/// A 12x8 blue PNG, as `Image.new('RGB', (12, 8), 'blue').save(image, format='PNG')`.
fn blue_png() -> Vec<u8> {
    let picture = image::RgbImage::from_pixel(12, 8, image::Rgb([0, 0, 255]));
    let mut out = std::io::Cursor::new(Vec::new());
    picture.write_to(&mut out, image::ImageFormat::Png).unwrap();
    out.into_inner()
}

/// `telegram_images.validate(data)`: the (bytes, suffix) a Codex task keeps as its browser picture.
fn validated(data: &[u8]) -> (Vec<u8>, String) {
    let image = eve_telegram_images::validate(data).expect("a valid picture");
    (image.data.clone(), image.suffix.clone())
}

/// `SimpleNamespace(provider='codex', browser_used=..., browser_tab_id='7', browser_screenshot=..., running=False)`
/// with `run` answering 'Example Domain is open.'.
struct CodexAgent {
    browser_used: bool,
    screenshot: Mutex<Option<(Vec<u8>, String)>>,
}

#[async_trait]
impl TaskAgent for CodexAgent {
    async fn run(&self, _goal: &str, _note: &str) -> anyhow::Result<String> {
        Ok("Example Domain is open.".into())
    }
    fn cancel(&self, _reason: &str) {}
    fn steer(&self, _text: &str) -> bool {
        false
    }
    fn provider(&self) -> String {
        "codex".into()
    }
    fn browser_used(&self) -> bool {
        self.browser_used
    }
    fn browser_screenshot(&self) -> Option<(Vec<u8>, String)> {
        self.screenshot.lock().clone()
    }
    fn browser_tab_id(&self) -> String {
        "7".into()
    }
}

// ---- the screen ----------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_a_plain_screenshot_request_is_answered_by_code_even_mid_task() {
    let _iso = isolated();
    let dir = tmp();
    let shot = dir.path().join("s.jpg");
    std::fs::write(&shot, b"\xff\xd8shot").unwrap();
    let api = FakeApi::new(vec![vec![said("open mail", 1)]]);
    let s = shot.clone();
    let rig = Rig::new(api.clone(), FakeCreate::new(vec![call("start_task", json!({"goal": "open mail"}), "call_1")]), config(), move |o| {
        o.screen = Some(Arc::new(move || Some(s.clone())));
    });
    run_rig(&rig, || async {
        assert!(rig.inlet.task_running());
        api.feed(vec![said("screenshot", 2), said("show me the screen", 3)]);
        settle(200).await;
        assert!(api.log.lock().photos.len() == 2 && rig.inlet.task_running()); // sent, and the task is untouched
        // a code word: the transcript's only (Python reads the turns the chat closed with; these are the same turns)
        assert!(!rig.inlet.turns().contains(&("user".to_string(), "screenshot".to_string())));
        rig.agents.last().release.set();
    })
    .await;
    assert_eq!(rig.create.requests.lock().len(), 1); // zero model calls for the two screens
    for asks in ["send me a screenshot", "what's on the screen?", "Show me your screen please"] {
        assert!(telegram::SCREEN_NOW.is_match(asks), "{asks}");
    }
    for goes_to_the_model in ["screenshot the headline on google news", "show me the top headline", "open mail"] {
        assert!(!telegram::SCREEN_NOW.is_match(goes_to_the_model), "{goes_to_the_model}");
    }
    let rig = Rig::new(FakeApi::new(vec![vec![said("screenshot", 1)]]), FakeCreate::new(vec![]), config(), |o| {
        o.screen = Some(Arc::new(|| None));
    });
    run_plain(&rig).await;
    let sent = rig.api.texts();
    assert!(!sent.is_empty() && sent[0].contains("couldn't grab the screen"), "{sent:?}");
}

#[tokio::test]
async fn test_codex_browser_picture_uses_task_bytes_not_desktop_and_cleans_up() {
    let _iso = isolated();
    let png = blue_png();
    let captured: Arc<Mutex<Vec<(i64, Vec<u8>, String, PathBuf)>>> = Arc::new(Mutex::new(Vec::new()));
    let c = captured.clone();
    let inner = FakeApi::new(vec![]);
    let api = Arc::new(Hooked {
        inner: inner.clone(),
        photo: Some(Box::new(move |chat, path, caption| {
            c.lock().push((chat, std::fs::read(path).unwrap(), caption.to_string(), path.to_path_buf()));
            Ok(())
        })),
        document: None,
    });
    let desktop = Arc::new(AtomicBool::new(false));
    let d = desktop.clone();
    let rig = rig_over(api, FakeCreate::new(vec![]), move |o| {
        // `screen=forbidden_desktop`: must not capture the unrelated desktop
        o.screen = Some(Arc::new(move || {
            d.store(true, Ordering::SeqCst);
            None
        }));
    });
    let agent = Arc::new(CodexAgent { browser_used: true, screenshot: Mutex::new(Some(validated(&png))) });
    rig.inlet.tt_set_agent(Some(agent.clone()));
    rig.inlet.tt_run_agent("open example.com", OWNER).await;
    assert!(captured.lock().is_empty()); // a browser task alone sends no picture
    rig.inlet.tt_run_agent("open example.com and show me the screen", OWNER).await;
    {
        let got = captured.lock();
        assert_eq!((got[0].0, got[0].1.clone()), (OWNER, png.clone()));
        assert!(got[0].2.contains("tab 7") && !got[0].3.exists(), "{:?}", got[0].2);
    }
    assert_eq!(rig.inlet.tt_send_screen(OWNER, "").await["source"], json!("task_browser"));
    assert_eq!(captured.lock().len(), 2);
    *agent.screenshot.lock() = None;
    let result = rig.inlet.tt_send_screen(OWNER, "").await;
    assert!(result.get("ok") != Some(&json!(true)) && captured.lock().len() == 2);
    rig.inlet.tt_run_agent("open example.com and show me the screen", OWNER).await;
    let last = inner.log.lock().sent.last().cloned().map(|(_, t)| t).unwrap_or_default();
    assert!(last.contains("couldn't send the task picture"), "{last}");
    assert_eq!(captured.lock().len(), 2);
    assert!(!desktop.load(Ordering::SeqCst), "must not capture the unrelated desktop");
}

#[tokio::test]
async fn test_browser_picture_delivery_failure_is_reported_and_temp_file_removed() {
    let _iso = isolated();
    let paths: Arc<Mutex<Vec<PathBuf>>> = Arc::new(Mutex::new(Vec::new()));
    let p = paths.clone();
    let api = Arc::new(Hooked {
        inner: FakeApi::new(vec![]),
        photo: Some(Box::new(move |_, path, _| {
            p.lock().push(path.to_path_buf());
            Err(BotApiError::new(0, "OSError")) // OSError('offline')
        })),
        document: None,
    });
    let rig = rig_over(api, FakeCreate::new(vec![]), |_| {});
    rig.inlet.tt_set_agent(Some(Arc::new(CodexAgent { browser_used: true, screenshot: Mutex::new(Some(validated(&blue_png()))) })));
    assert!(rig.inlet.tt_send_screen(OWNER, "").await.get("ok") != Some(&json!(true)));
    let paths = paths.lock();
    assert!(!paths.is_empty() && !paths[0].exists());
}

/// `SimpleNamespace(provider='codex', browser_used=False, running=False)`.
struct NativeCodex;

#[async_trait]
impl TaskAgent for NativeCodex {
    async fn run(&self, _goal: &str, _note: &str) -> anyhow::Result<String> {
        Ok(String::new())
    }
    fn cancel(&self, _reason: &str) {}
    fn steer(&self, _text: &str) -> bool {
        false
    }
    fn provider(&self) -> String {
        "codex".into()
    }
}

#[tokio::test]
async fn test_finished_native_codex_task_can_still_send_desktop_picture() {
    let _iso = isolated();
    let dir = tmp();
    let shot = dir.path().join("native.jpg");
    std::fs::write(&shot, b"native picture").unwrap();
    let s = shot.clone();
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), move |o| {
        o.screen = Some(Arc::new(move || Some(s.clone())));
    });
    rig.inlet.tt_set_agent(Some(Arc::new(NativeCodex)));
    assert_eq!(rig.inlet.tt_send_screen(OWNER, "native task").await["ok"], json!(true));
    assert_eq!(rig.api.log.lock().photos, vec![(OWNER, shot.to_string_lossy().into_owned(), "native task".to_string())]);
    assert!(!shot.exists());
}

// ---- files ---------------------------------------------------------------------------------------------------

#[test]
fn test_files_leave_only_from_the_owners_home_and_never_from_a_hidden_folder() {
    let dir = tmp();
    let tmp_path = eve_transcripts::py::resolve(dir.path()).unwrap_or_else(|_| dir.path().to_path_buf());
    let home = tmp_path.join("home");
    std::fs::create_dir_all(home.join("Desktop")).unwrap();
    std::fs::create_dir(home.join(".ssh")).unwrap();
    std::fs::write(home.join(".ssh").join("id_ed25519"), "SECRET").unwrap();
    std::fs::write(home.join("Desktop").join("report.pdf"), b"%PDF").unwrap();
    std::fs::write(home.join("Desktop").join(".hidden.txt"), "x").unwrap();
    std::fs::write(tmp_path.join("outside.txt"), "x").unwrap();
    std::os::unix::fs::symlink(tmp_path.join("outside.txt"), home.join("Desktop").join("link")).unwrap();
    std::os::unix::fs::symlink(home.join(".ssh").join("id_ed25519"), home.join("Desktop").join("sshlink")).unwrap();
    let report = home.join("Desktop").join("report.pdf");
    let (ok, why) = telegram::resolve_owner_path(&report.to_string_lossy(), Some(&home));
    assert_eq!(ok, Some(std::fs::canonicalize(&report).unwrap()));
    assert_eq!(why, "");
    let s = |p: PathBuf| p.to_string_lossy().into_owned();
    for (bad, reason) in [
        (s(home.join(".ssh").join("id_ed25519")), "hidden"),
        (s(home.join("Desktop").join(".hidden.txt")), "hidden"),
        (s(tmp_path.join("outside.txt")), "home folder"),
        (s(home.join("Desktop").join("link")), "home folder"),
        (s(home.join("Desktop").join("sshlink")), "hidden"),
        ("../../etc/passwd".to_string(), "home folder"),
        (String::new(), "no path"),
        (s(home.join("nope.txt")), "no such"),
    ] {
        let (real, why) = telegram::resolve_owner_path(&bad, Some(&home));
        assert!(real.is_none() && why.contains(reason), "{bad:?} {why:?}");
    }
    let listing = telegram::list_files(&s(home.join("Desktop")), Some(&home));
    let names: Vec<String> = listing["files"].as_array().unwrap().iter().map(|f| f["name"].as_str().unwrap().to_string()).collect();
    assert!(!names.is_empty() && !names.contains(&".hidden.txt".to_string()));
    assert_eq!(telegram::list_files(&s(home.join(".ssh")), Some(&home))["ok"], json!(false));
    assert_eq!(telegram::list_files(&s(report), Some(&home))["ok"], json!(false));
}

#[tokio::test]
async fn test_send_file_sends_a_document_and_refuses_folders_and_big_files() {
    let _iso = isolated();
    let dir = tmp();
    let home = dir.path().join("home");
    std::fs::create_dir_all(home.join("Documents")).unwrap();
    let doc = home.join("Documents").join("notes.txt");
    std::fs::write(&doc, "hello").unwrap();
    let big = home.join("Documents").join("big.bin");
    // `MAX_DOCUMENT_BYTES = 0` over a one-byte file: here a sparse file one byte past the real limit
    std::fs::File::create(&big).unwrap().set_len(MAX_DOCUMENT_BYTES as u64 + 1).unwrap();
    let sent: Arc<Mutex<Vec<(i64, String, String)>>> = Arc::new(Mutex::new(Vec::new()));
    let with_home = |o: &mut InletOptions| {
        let mut environ = conftest_env();
        environ.insert("HOME".into(), home.to_string_lossy().into_owned()); // `Path.home()` is `home`
        o.environ = Some(environ);
    };
    let api = |batch: Vec<Obj>| -> Arc<dyn TelegramApi> {
        let s = sent.clone();
        Arc::new(Hooked {
            inner: FakeApi::new(vec![batch]),
            photo: None,
            document: Some(Box::new(move |chat, path, caption| {
                s.lock().push((chat, path.to_string_lossy().into_owned(), caption.to_string()));
                Ok(())
            })),
        })
    };
    let rig = rig_over(
        api(vec![said("send me my notes", 1)]),
        FakeCreate::new(vec![call("send_file", json!({"path": "~/Documents/notes.txt", "caption": "Your notes"}), "call_1"), reply("Sent.")]),
        with_home,
    );
    run_plain(&rig).await;
    let real = eve_transcripts::py::resolve(&doc).unwrap();
    assert_eq!(*sent.lock(), vec![(OWNER, real.to_string_lossy().into_owned(), "Your notes".to_string())]);
    assert_eq!(output_of(&rig, 1), json!({"ok": true, "sent": "notes.txt", "bytes": 5}));
    for (path, reason) in [("~/Documents", "folder"), ("~/Documents/big.bin", "too big"), ("~/.ssh/x", "hidden")] {
        let rig = rig_over(
            api(vec![said("send it", 1)]),
            FakeCreate::new(vec![call("send_file", json!({"path": path, "caption": ""}), "call_1"), reply("No.")]),
            with_home,
        );
        run_plain(&rig).await;
        let out = output_of(&rig, 1);
        assert!(out["reason"].as_str().unwrap_or("").contains(reason), "{path}: {out}");
    }
    assert_eq!(sent.lock().len(), 1);

    let transport = MockTransport::ok();
    let bot_api = bot(&transport);
    bot_api.send_document(OWNER, &doc, "c").await.unwrap();
    bot_api.close().await;
    let seen = transport.requests.lock();
    assert!(seen[0].url.ends_with("/sendDocument"));
    assert!(contains(&seen[0].body, br#"filename="notes.txt""#) && contains(&seen[0].body, b"hello"));
}

// ---- notes on the phone --------------------------------------------------------------------------------------

/// test_telegram.py's `FakeNotes`.
#[derive(Default)]
struct FakeNotes {
    calls: Mutex<Vec<String>>,
}

#[async_trait]
impl RoomNotes for FakeNotes {
    fn start(&self) -> Value {
        self.calls.lock().push("start".into());
        json!({"ok": true, "started": true})
    }
    async fn stop(&self, reason: &str) -> Value {
        self.calls.lock().push(format!("stop:{reason}"));
        json!({"ok": true, "minutes": 3})
    }
    fn status(&self) -> Obj {
        obj(json!({"active": false}))
    }
}

#[tokio::test]
async fn test_take_notes_asks_the_phone_first_and_the_room_answers_what_is_not_the_phones() {
    let _iso = isolated();
    let notes = Arc::new(FakeNotes::default());
    let n = notes.clone();
    let rig = Rig::new(FakeApi::new(vec![]), FakeCreate::new(vec![]), config(), move |o| {
        o.notes = Some(Arc::new(move || n.clone() as Arc<dyn RoomNotes>));
    });
    let asked: Arc<Mutex<Vec<(String, bool)>>> = Arc::new(Mutex::new(Vec::new()));
    let phone_answer: Arc<Mutex<Obj>> = Arc::new(Mutex::new(Obj::new()));
    let (a, answers) = (asked.clone(), phone_answer.clone());
    let phone: PhoneNotes = Arc::new(move |action: String, call_say, _later| {
        a.lock().push((action.clone(), call_say.is_some()));
        let answer = answers.lock().get(&action).and_then(|v| v.as_object().cloned());
        async move { answer }.boxed()
    });
    *rig.inlet.phone_notes.lock() = Some(phone);
    let start = obj(json!({"action": "start"}));
    let room = rig.inlet.tt_robot_tool("take_notes", &start).await; // not the phone's: the room
    phone_answer.lock().insert("start".into(), json!({"ok": true, "started": true, "on": "your phone"}));
    let on_phone = rig.inlet.tt_robot_tool("take_notes", &start).await;
    assert_eq!(Value::Object(room), json!({"ok": true, "started": true}));
    assert_eq!(*notes.calls.lock(), vec!["start".to_string()]);
    assert_eq!(on_phone["on"], json!("your phone"));
    assert_eq!(*notes.calls.lock(), vec!["start".to_string()]); // the room was not started again
    assert_eq!(*asked.lock(), vec![("start".to_string(), false), ("start".to_string(), false)]); // a text: no call's reader
}

// ---- no emoji ------------------------------------------------------------------------------------------------

#[tokio::test]
async fn test_no_emoji_leaves_the_mac() {
    assert_eq!(telegram::plain("On it! \u{1F680}\u{1F4BB} Done \u{2705} ok \u{1F44D}\u{1F3FD}"), "On it! Done ok");
    assert_eq!(telegram::plain("plain words, 3 + 4 = 7, café, 日本語"), "plain words, 3 + 4 = 7, café, 日本語");
    assert_eq!(telegram::plain("keycap 1\u{fe0f}\u{20e3} flag \u{1F1FA}\u{1F1F8}"), "keycap 1 flag");
    assert_eq!(telegram::plain("On it \u{2014} the result comes later"), "On it, the result comes later");
    assert_eq!(telegram::plain("Done \u{2013} both of them."), "Done, both of them.");
    assert!(telegram::plain("It worked\u{2014}.") == "It worked." && telegram::plain("well-known") == "well-known");

    let transport = MockTransport::ok();
    let api = bot(&transport);
    api.send_message(OWNER, "Calculator is open \u{1F9EE}\u{2728}", None, None, &[], 0, None).await.unwrap();
    let this_file = PathBuf::from(file!());
    let this_file = if this_file.exists() { this_file } else { Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("telegram_more.rs") };
    api.send_document(OWNER, &this_file, "here \u{1F4CE}").await.unwrap();
    api.close().await;
    let requests = transport.requests.lock();
    let bodies: Vec<String> = requests
        .iter()
        .map(|r| {
            if content_type(r).starts_with("application/json") {
                serde_json::from_slice::<Value>(&r.body).unwrap()["text"].as_str().unwrap_or("").to_string()
            } else {
                String::from_utf8_lossy(&r.body).into_owned()
            }
        })
        .collect();
    let (sent, doc) = (&bodies[0], &bodies[1]);
    assert!(sent == "Calculator is open" && !doc.contains('\u{1F4CE}') && doc.contains("here"), "{sent:?}");
}
