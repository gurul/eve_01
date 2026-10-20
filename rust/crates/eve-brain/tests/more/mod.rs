//! What this crate's later test files share beyond `support` (architect-owned): the spend ledger's isolation,
//! `run_rig`, a Rig over an API that is not a FakeApi, a FakeApi with methods of its own, an httpx.MockTransport and
//! test_telegram.py's FakeMemory and FakeNotes. Include with `mod support; mod more;`.

#![allow(dead_code, unused_imports)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_brain::prelude::{BrainMemory, Http, HttpError, HttpRequest, HttpResponse, HttpTransport, RoomNotes};
use eve_brain::telegram::{ApiImageError, BotApi, BotApiError, InletOptions, TelegramApi, TelegramInlet};

use crate::support::*;

pub const TOKEN: &str = "123456:AAsecretTOKENvalue";

// ---- what every test here shares ------------------------------------------------------------------------------

/// conftest.py's `_spend_ledger_in_tmp`: the ledger is process-wide, so the tests that run an inlet take this lock
/// and each gets a ledger folder of its own.
pub static SERIAL: Mutex<()> = Mutex::new(());

pub struct Isolated {
    _lock: parking_lot::MutexGuard<'static, ()>,
    _ledger: tempfile::TempDir,
}

impl Drop for Isolated {
    fn drop(&mut self) {
        eve_spend::set_dir(None);
    }
}

pub fn isolated() -> Isolated {
    let lock = SERIAL.lock();
    let ledger = tmp();
    eve_spend::set_dir(Some(ledger.path().to_path_buf()));
    Isolated { _lock: lock, _ledger: ledger }
}

pub fn obj(v: Value) -> Obj {
    v.as_object().cloned().expect("an object")
}

/// `update(text, update_id=n)` from the owner, now.
pub fn said(text: &str, update_id: i64) -> Obj {
    update(update_id, text, OWNER, NOW, update_id)
}

/// test_telegram.py's `run_rig`: the poll loop runs, `during` runs while it does, then it is stopped. The loop must
/// still be running when it is stopped.
pub async fn run_rig<F, Fut>(rig: &Rig, during: F)
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

pub async fn run_plain(rig: &Rig) {
    run_rig(rig, || async {}).await;
}

/// `Rig(Api(...), create, **kw)` over an API that is not a FakeApi: the same lent fakes as `Rig::new`, `tweak` sets
/// the rest of the keyword arguments, and `rig.api` is an unused FakeApi.
pub fn rig_over(other: Arc<dyn TelegramApi>, create: Arc<FakeCreate>, tweak: impl FnOnce(&mut InletOptions)) -> Rig {
    rig_over_config(other, create, config(), tweak)
}

/// The same with a config of its own.
pub fn rig_over_config(other: Arc<dyn TelegramApi>, create: Arc<FakeCreate>, cfg: eve_brain::telegram::TelegramConfig, tweak: impl FnOnce(&mut InletOptions)) -> Rig {
    let made = Rig::new(FakeApi::new(vec![]), create.clone(), cfg.clone(), |_| {});
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
    let inlet = TelegramInlet::new(cfg, other, create.create(), options);
    Rig { inlet, ..made }
}

/// `json.loads(rig.create.requests[n]["input"][-1]["output"])`.
pub fn output_of(rig: &Rig, n: usize) -> Value {
    let requests = rig.create.requests.lock();
    let items = requests[n]["input"].as_array().expect("input");
    serde_json::from_str(items.last().unwrap()["output"].as_str().expect("an output")).unwrap()
}

// ---- a FakeApi with send_photo or send_document of its own (Python's `class Api(FakeApi)`) ---------------------

/// `send_photo` / `send_document` of its own.
pub type FileHook = Box<dyn Fn(i64, &Path, &str) -> Result<(), BotApiError> + Send + Sync>;
/// `react` of its own.
pub type ReactHook = Box<dyn Fn(i64, i64, &str) -> Result<(), BotApiError> + Send + Sync>;

pub struct Hooked {
    pub inner: Arc<FakeApi>,
    pub photo: Option<FileHook>,
    pub document: Option<FileHook>,
    pub react: Option<ReactHook>,
    /// awaited before each `send_inline` goes on to the FakeApi
    pub before_inline: Option<Box<dyn Fn() -> futures::future::BoxFuture<'static, ()> + Send + Sync>>,
}

impl Hooked {
    /// `class Api(FakeApi)` with nothing of its own yet.
    pub fn new(inner: Arc<FakeApi>) -> Self {
        Self { inner, photo: None, document: None, react: None, before_inline: None }
    }
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
        if let Some(wait) = &self.before_inline {
            wait().await;
        }
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
        match &self.react {
            Some(hook) => hook(chat_id, message_id, emoji),
            None => self.inner.react(chat_id, message_id, emoji).await,
        }
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
pub struct MockTransport {
    pub requests: Mutex<Vec<HttpRequest>>,
}

impl MockTransport {
    pub fn ok() -> Arc<Self> {
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

pub fn bot(transport: &Arc<MockTransport>) -> BotApi {
    let http: Http = transport.clone();
    BotApi::with_http(TOKEN, http)
}

pub fn content_type(request: &HttpRequest) -> String {
    request.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-type")).map(|(_, v)| v.clone()).unwrap_or_default()
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}


// ---- test_telegram.py's FakeMemory and FakeNotes --------------------------------------------------------------

/// test_telegram.py's MEMORY_TOOL_SCHEMAS.
pub fn memory_tool_schemas() -> Vec<Value> {
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
pub struct FakeMemory {
    pub transcripts: Arc<eve_transcripts::Transcripts>,
    pub profile_text: String,
    pub stars: Mutex<Vec<String>>,
    pub calls: Mutex<Vec<(String, Value, String)>>,
}

impl FakeMemory {
    pub fn new(root: &Path, profile: &str) -> Arc<Self> {
        std::fs::create_dir_all(root).unwrap();
        let config = eve_transcripts::TranscriptConfig { enabled: true, root: root.join("transcripts"), home: Some(root.to_path_buf()) };
        let transcripts = Arc::new(eve_transcripts::Transcripts::new(config, None, None));
        Arc::new(Self { transcripts, profile_text: profile.to_string(), stars: Mutex::new(Vec::new()), calls: Mutex::new(Vec::new()) })
    }

    /// Every transcript line written today, in order.
    pub fn said(&self) -> Vec<Map<String, Value>> {
        let day = self.transcripts.day_of(self.transcripts.now());
        self.transcripts.lines(&day).iter().map(|l| (**l).clone()).collect()
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

/// test_telegram.py's `FakeNotes`.
#[derive(Default)]
pub struct FakeNotes {
    pub calls: Mutex<Vec<String>>,
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

