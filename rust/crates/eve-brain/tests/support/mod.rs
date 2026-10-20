//! The fakes the Python tests share (test_telegram.py's FakeApi, FakeCreate, FakeAgent and Rig; test_voice_agent.py's
//! fake connection and speaker), ported once so every unit's tests drive the same ones.
//!
//! Architect-owned: a unit that needs more puts it in its own tests file. Include with `mod support;`.

#![allow(dead_code, unused_imports)]

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::future::BoxFuture;
use futures::FutureExt;
use parking_lot::Mutex;
use serde_json::{json, Map, Value};

use eve_agent_contract::AgentEvent;
use eve_brain::prelude::{AgentFactory, AskUser, BrowserShot, Clock, Create, Flag, LiveConnection, OnEvent, Sleep, TaskAgent, VoiceSpeaker};
use eve_brain::telegram::{ApiImageError, BotApiError, InletOptions, TelegramApi, TelegramConfig, TelegramInlet};

pub type Obj = Map<String, Value>;

/// A Bot API refusal, as the tests raise one.
pub fn api_error(code: i64, description: &str) -> BotApiError {
    BotApiError { message: format!("{code}: {description}"), code, description: description.to_string() }
}

// ---- FakeApi ------------------------------------------------------------------------------------------------------

/// One scripted poll result: a batch of updates, or the poll failing.
pub enum Batch {
    Updates(Vec<Obj>),
    Fail(BotApiError),
}

/// What FakeApi recorded (Python's attributes of the same names).
#[derive(Default)]
pub struct ApiLog {
    pub offsets: Vec<Option<i64>>,
    pub sent: Vec<(i64, String)>,
    /// (title, subtitle, body) as passed
    pub titled: Vec<(Option<String>, Option<String>, String)>,
    pub photos: Vec<(i64, String, String)>,
    pub documents: Vec<(i64, String, String)>,
    /// per message sent, [] for none
    pub buttons: Vec<Vec<String>>,
    pub reactions: Vec<(i64, i64, String)>,
    pub typings: Vec<i64>,
    pub commands: Vec<(Vec<(String, String)>, i64)>,
    /// "typing" / "send" / "draft", as they happened
    pub order: Vec<String>,
    pub polls: i64,
    /// the last inline message's id (1000 at first)
    pub message_id: i64,
    /// (message id, rows) per inline send
    pub keyboards: Vec<(i64, Vec<Vec<(String, String, String)>>)>,
    pub edits: Vec<(i64, String)>,
    /// editMessageReplyMarkup: buttons taken off
    pub dropped: Vec<i64>,
    /// answerCallbackQuery (query id, toast)
    pub answered: Vec<(String, String)>,
    /// (text, reply_to) per message that replied
    pub replies: Vec<(String, i64)>,
    /// (text, placeholder) per ForceReply send
    pub force_replies: Vec<(String, String)>,
    /// (message id, rows or None) per edit
    pub edit_keyboards: Vec<(i64, Option<Vec<Vec<(String, String, String)>>>)>,
    /// sendMessageDraft (chat, draft id, text)
    pub drafts: Vec<(i64, i64, String)>,
    /// send_web_apps / send_url_buttons / send_photo_web_apps (chat, text, buttons)
    pub web_apps: Vec<(i64, String, Vec<(String, String)>)>,
    pub url_buttons: Vec<(i64, String, Vec<(String, String)>)>,
    pub photo_web_apps: Vec<(i64, Vec<u8>, String, Vec<(String, String)>)>,
}

/// Hands out scripted poll results, then holds the poll open like Telegram does.
pub struct FakeApi {
    pub batches: Mutex<VecDeque<Batch>>,
    more: tokio::sync::Notify,
    pub log: Mutex<ApiLog>,
    /// editMessageText refused with this
    pub fail_edits: Mutex<Option<BotApiError>>,
    /// reply_to / force_reply refused (400)
    pub refuse_links: AtomicBool,
    /// sendMessageDraft refused with this
    pub fail_drafts: Mutex<Option<BotApiError>>,
    /// sendMessage with an inline keyboard refused with this (nothing is logged)
    pub fail_inline: Mutex<Option<BotApiError>>,
    /// receive_image's answer (None: an ImageError-free 1x1 JPEG)
    pub image: Mutex<Option<Result<eve_telegram_images::ReceivedImage, String>>>,
}

impl FakeApi {
    pub fn new(batches: Vec<Vec<Obj>>) -> Arc<Self> {
        Arc::new(Self {
            batches: Mutex::new(batches.into_iter().map(Batch::Updates).collect()),
            more: tokio::sync::Notify::new(),
            log: Mutex::new(ApiLog { message_id: 1000, ..ApiLog::default() }),
            fail_edits: Mutex::new(None),
            refuse_links: AtomicBool::new(false),
            fail_drafts: Mutex::new(None),
            fail_inline: Mutex::new(None),
            image: Mutex::new(None),
        })
    }

    /// A message that arrives later, while the poll is being held open.
    pub fn feed(&self, updates: Vec<Obj>) {
        self.batches.lock().push_back(Batch::Updates(updates));
        self.more.notify_waiters();
        self.more.notify_one();
    }

    /// The next poll fails with `error`.
    pub fn feed_error(&self, error: BotApiError) {
        self.batches.lock().push_back(Batch::Fail(error));
        self.more.notify_waiters();
        self.more.notify_one();
    }

    /// (message id, callback key) of the button with this label on the `message`-th inline keyboard (-1: the last).
    pub fn key(&self, label: &str, message: i64) -> (i64, String) {
        let log = self.log.lock();
        let i = if message < 0 { log.keyboards.len() as i64 + message } else { message } as usize;
        let (id, rows) = &log.keyboards[i];
        let data = rows.iter().flatten().find(|(text, _, _)| text == label).map(|(_, d, _)| d.clone());
        (*id, data.unwrap_or_else(|| panic!("no button {label:?}")))
    }

    /// The texts sent, in order.
    pub fn texts(&self) -> Vec<String> {
        self.log.lock().sent.iter().map(|(_, t)| t.clone()).collect()
    }
}

#[async_trait]
impl TelegramApi for FakeApi {
    async fn get_me(&self) -> Result<Obj, BotApiError> {
        Ok(json!({"id": 1, "is_bot": true, "username": "eve_test_bot"}).as_object().unwrap().clone())
    }

    async fn get_updates(&self, offset: Option<i64>, _timeout: i64) -> Result<Vec<Obj>, BotApiError> {
        {
            let mut log = self.log.lock();
            log.offsets.push(offset);
            log.polls += 1;
        }
        loop {
            let notified = self.more.notified();
            if let Some(batch) = self.batches.lock().pop_front() {
                return match batch {
                    Batch::Updates(updates) => Ok(updates),
                    Batch::Fail(e) => Err(e),
                };
            }
            notified.await;
        }
    }

    async fn receive_image(
        &self,
        _item: &eve_telegram_images::Attachment,
    ) -> Result<eve_telegram_images::ReceivedImage, ApiImageError> {
        match self.image.lock().clone() {
            Some(Ok(image)) => Ok(image),
            Some(Err(description)) => Err(ApiImageError::Api(api_error(400, &description))),
            None => Ok(eve_telegram_images::ReceivedImage::new(vec![0xFF, 0xD8, 0xFF, 0xD9], "image/jpeg", ".jpg")),
        }
    }

    async fn send_message(
        &self,
        chat_id: i64,
        text: &str,
        title: Option<&str>,
        subtitle: Option<&str>,
        buttons: &[String],
        reply_to: i64,
        force_reply: Option<&str>,
    ) -> Result<(), BotApiError> {
        if self.refuse_links.load(Ordering::SeqCst) && (reply_to != 0 || force_reply.is_some()) {
            return Err(api_error(400, "Bad Request: message to be replied not found"));
        }
        let mut log = self.log.lock();
        if reply_to != 0 {
            log.replies.push((text.to_string(), reply_to));
        }
        if let Some(placeholder) = force_reply {
            log.force_replies.push((text.to_string(), placeholder.to_string()));
        }
        log.sent.push((chat_id, text.to_string()));
        log.titled.push((title.map(str::to_string), subtitle.map(str::to_string), text.to_string()));
        log.buttons.push(buttons.to_vec());
        log.order.push("send".into());
        Ok(())
    }

    async fn send_inline(
        &self,
        chat_id: i64,
        text: &str,
        keyboard: &[Vec<(String, String, String)>],
        title: Option<&str>,
        subtitle: Option<&str>,
    ) -> Result<i64, BotApiError> {
        if let Some(e) = self.fail_inline.lock().clone() {
            return Err(e);
        }
        let mut log = self.log.lock();
        log.sent.push((chat_id, text.to_string()));
        log.titled.push((title.map(str::to_string), subtitle.map(str::to_string), text.to_string()));
        log.buttons.push(keyboard.iter().flatten().map(|(label, _, _)| label.clone()).collect());
        log.order.push("send".into());
        log.message_id += 1;
        let id = log.message_id;
        log.keyboards.push((id, keyboard.to_vec()));
        Ok(id)
    }

    async fn edit_message(
        &self,
        _chat_id: i64,
        message_id: i64,
        text: &str,
        _title: Option<&str>,
        _subtitle: Option<&str>,
        keyboard: Option<&[Vec<(String, String, String)>]>,
    ) -> Result<(), BotApiError> {
        if let Some(e) = self.fail_edits.lock().clone() {
            return Err(e);
        }
        let mut log = self.log.lock();
        log.edits.push((message_id, text.to_string()));
        log.edit_keyboards.push((message_id, keyboard.map(|k| k.to_vec())));
        Ok(())
    }

    async fn drop_buttons(&self, _chat_id: i64, message_id: i64) -> Result<(), BotApiError> {
        self.log.lock().dropped.push(message_id);
        Ok(())
    }

    async fn answer_callback(&self, query_id: &str, text: &str) -> Result<(), BotApiError> {
        self.log.lock().answered.push((query_id.to_string(), text.to_string()));
        Ok(())
    }

    async fn react(&self, chat_id: i64, message_id: i64, emoji: &str) -> Result<(), BotApiError> {
        self.log.lock().reactions.push((chat_id, message_id, emoji.to_string()));
        Ok(())
    }

    async fn send_photo(&self, chat_id: i64, path: &Path, caption: &str) -> Result<(), BotApiError> {
        self.log.lock().photos.push((chat_id, path.display().to_string(), caption.to_string()));
        Ok(())
    }

    async fn send_document(&self, chat_id: i64, path: &Path, caption: &str) -> Result<(), BotApiError> {
        self.log.lock().documents.push((chat_id, path.display().to_string(), caption.to_string()));
        Ok(())
    }

    async fn typing(&self, chat_id: i64) -> Result<(), BotApiError> {
        let mut log = self.log.lock();
        log.typings.push(chat_id);
        log.order.push("typing".into());
        Ok(())
    }

    async fn send_draft(&self, chat_id: i64, draft_id: i64, text: &str) -> Result<(), BotApiError> {
        if let Some(e) = self.fail_drafts.lock().clone() {
            return Err(e);
        }
        let mut log = self.log.lock();
        log.drafts.push((chat_id, draft_id, text.to_string()));
        log.order.push("draft".into());
        Ok(())
    }

    async fn send_web_apps(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.log.lock().web_apps.push((chat_id, text.to_string(), buttons.to_vec()));
        Ok(())
    }

    async fn send_url_buttons(&self, chat_id: i64, text: &str, buttons: &[(String, String)]) -> Result<(), BotApiError> {
        self.log.lock().url_buttons.push((chat_id, text.to_string(), buttons.to_vec()));
        Ok(())
    }

    async fn send_photo_web_apps(
        &self,
        chat_id: i64,
        jpeg: &[u8],
        caption: &str,
        buttons: &[(String, String)],
    ) -> Result<(), BotApiError> {
        self.log.lock().photo_web_apps.push((chat_id, jpeg.to_vec(), caption.to_string(), buttons.to_vec()));
        Ok(())
    }

    async fn set_commands(&self, commands: &[(String, String)], chat_id: i64) -> Result<(), BotApiError> {
        self.log.lock().commands.push((commands.to_vec(), chat_id));
        Ok(())
    }
}

// ---- FakeCreate ---------------------------------------------------------------------------------------------------

/// One scripted model response: a body, a failure, or a body made when it is asked for.
pub enum Scripted {
    Body(Value),
    Fail(String),
    Later(Box<dyn FnOnce() -> BoxFuture<'static, anyhow::Result<Value>> + Send>),
}

/// The model, scripted: each call takes the next response; a call nobody scripted fails the test.
pub struct FakeCreate {
    pub responses: Mutex<VecDeque<Scripted>>,
    pub requests: Mutex<Vec<Value>>,
}

impl FakeCreate {
    pub fn new(responses: Vec<Value>) -> Arc<Self> {
        Arc::new(Self {
            responses: Mutex::new(responses.into_iter().map(Scripted::Body).collect()),
            requests: Mutex::new(Vec::new()),
        })
    }

    pub fn push(&self, response: Scripted) {
        self.responses.lock().push_back(response);
    }

    /// As the inlet's `create`.
    pub fn create(self: &Arc<Self>) -> Create {
        let me = self.clone();
        Arc::new(move |request: Value| {
            me.requests.lock().push(request);
            let next = me.responses.lock().pop_front();
            async move {
                match next {
                    // raised, as Python's FakeCreate raises AssertionError: the turn answers it as any failure
                    None => Err(anyhow::anyhow!("a model call nobody scripted")),
                    Some(Scripted::Body(body)) => Ok(body),
                    Some(Scripted::Fail(message)) => Err(anyhow::anyhow!(message)),
                    Some(Scripted::Later(make)) => make().await,
                }
            }
            .boxed()
        })
    }
}

/// A Responses API reply that only says `text` (test_telegram.py's `say`).
pub fn reply(text: &str) -> Value {
    json!({"id": "resp", "output": [{"type": "message", "role": "assistant",
                                     "content": [{"type": "output_text", "text": text}]}]})
}

/// A Responses API reply that calls one tool, its reasoning item first (test_telegram.py's `call`): a stateless
/// round must carry both back.
pub fn call(name: &str, args: Value, call_id: &str) -> Value {
    json!({"id": "resp", "output": [{"type": "reasoning", "id": "rs_1", "encrypted_content": "opaque"},
                                    {"type": "function_call", "name": name, "call_id": call_id,
                                     "arguments": serde_json::to_string(&args).unwrap()}]})
}

// ---- FakeAgent ----------------------------------------------------------------------------------------------------

/// A computer task that waits to be released (or cancelled), asking its question first when it has one.
pub struct FakeAgent {
    pub on_event: OnEvent,
    pub ask_user: AskUser,
    pub final_text: String,
    pub ask_q: Option<String>,
    pub running: AtomicBool,
    pub goal: Mutex<Option<String>>,
    pub note: Mutex<String>,
    pub steers: Mutex<Vec<String>>,
    pub cancel_reason: Mutex<Option<String>>,
    pub release: Flag,
    pub floor: Option<String>,
}

#[async_trait]
impl TaskAgent for FakeAgent {
    async fn run(&self, goal: &str, note: &str) -> anyhow::Result<String> {
        self.running.store(true, Ordering::SeqCst);
        *self.goal.lock() = Some(goal.to_string());
        *self.note.lock() = note.to_string();
        (self.on_event)(&AgentEvent::with_text("started", goal));
        let mut answer = None;
        if let Some(q) = &self.ask_q {
            (self.on_event)(&AgentEvent::with_turn("ask", q.clone(), 1));
            answer = Some((self.ask_user)(q.clone()).await?);
        }
        self.release.wait().await;
        self.running.store(false, Ordering::SeqCst);
        if self.cancel_reason.lock().is_some() {
            (self.on_event)(&AgentEvent::with_text("cancelled", "Stopped."));
            return Ok("Okay, I stopped.".into());
        }
        (self.on_event)(&AgentEvent::with_turn("final", self.final_text.clone(), 2));
        Ok(match answer {
            Some(a) => format!("{} (answer={a})", self.final_text),
            None => self.final_text.clone(),
        })
    }

    fn cancel(&self, reason: &str) {
        *self.cancel_reason.lock() = Some(reason.to_string());
        self.release.set();
    }

    fn steer(&self, text: &str) -> bool {
        self.steers.lock().push(text.to_string());
        self.running.load(Ordering::SeqCst)
    }

    fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }
}

/// The agents a factory made, newest last.
#[derive(Default)]
pub struct Agents {
    pub made: Mutex<Vec<Arc<FakeAgent>>>,
    pub final_text: Mutex<Option<String>>,
    pub ask: Mutex<Option<String>>,
}

impl Agents {
    pub fn factory(self: &Arc<Self>) -> AgentFactory {
        let me = self.clone();
        Arc::new(move |on_event: OnEvent, ask_user: AskUser, floor: Option<String>| {
            let agent = Arc::new(FakeAgent {
                on_event,
                ask_user,
                final_text: me.final_text.lock().clone().unwrap_or_else(|| "Calculator is open.".into()),
                ask_q: me.ask.lock().clone(),
                running: AtomicBool::new(false),
                goal: Mutex::new(None),
                note: Mutex::new(String::new()),
                steers: Mutex::new(Vec::new()),
                cancel_reason: Mutex::new(None),
                release: Flag::new(),
                floor,
            });
            me.made.lock().push(agent.clone());
            agent as Arc<dyn TaskAgent>
        })
    }

    pub fn last(&self) -> Arc<FakeAgent> {
        self.made.lock().last().cloned().expect("no agent was made")
    }
}

// ---- a settable clock and a sleep that only yields ---------------------------------------------------------------

#[derive(Clone)]
pub struct FakeClock(pub Arc<Mutex<f64>>);

impl FakeClock {
    pub fn new(t: f64) -> Self {
        Self(Arc::new(Mutex::new(t)))
    }
    pub fn set(&self, t: f64) {
        *self.0.lock() = t;
    }
    pub fn advance(&self, secs: f64) {
        *self.0.lock() += secs;
    }
    pub fn now(&self) -> f64 {
        *self.0.lock()
    }
    pub fn clock(&self) -> Clock {
        let me = self.clone();
        Arc::new(move || me.now())
    }
}

/// `async def no_sleep(_secs): await asyncio.sleep(0)`.
pub fn no_sleep() -> Sleep {
    Arc::new(|_secs: f64| async { tokio::task::yield_now().await }.boxed())
}

/// Let the spawned jobs run (`await asyncio.sleep(0)` a few times).
pub async fn settle(rounds: usize) {
    for _ in 0..rounds {
        tokio::task::yield_now().await;
    }
}

// ---- the Telegram update builders ---------------------------------------------------------------------------------

pub const OWNER: i64 = 42;
/// test_telegram.py's NOW: 2026-09-21 12:00 UTC.
pub const NOW: f64 = 1_790_000_000.0;

/// A text message from `user` in their private chat.
pub fn update(update_id: i64, text: &str, user: i64, date: f64, message_id: i64) -> Obj {
    json!({"update_id": update_id,
           "message": {"message_id": message_id, "date": date as i64, "text": text,
                       "chat": {"id": user, "type": "private"},
                       "from": {"id": user, "is_bot": false, "first_name": "Owner"}}})
    .as_object()
    .unwrap()
    .clone()
}

/// A tap on an inline button (callback_query).
pub fn tap_update(update_id: i64, message_id: i64, data: &str, user: i64, query_id: &str) -> Obj {
    json!({"update_id": update_id,
           "callback_query": {"id": query_id, "data": data,
                              "from": {"id": user, "is_bot": false, "first_name": "Owner"},
                              "message": {"message_id": message_id, "chat": {"id": user, "type": "private"}}}})
    .as_object()
    .unwrap()
    .clone()
}

/// The owner's config: enabled, one owner.
pub fn config() -> TelegramConfig {
    TelegramConfig { enabled: true, token: "123:abc".into(), owner_ids: vec![OWNER], ..TelegramConfig::default() }
}

// ---- Rig ----------------------------------------------------------------------------------------------------------

/// One inlet with everything it is lent faked, a settable clock, and the agents it made.
pub struct Rig {
    pub api: Arc<FakeApi>,
    pub create: Arc<FakeCreate>,
    pub agents: Arc<Agents>,
    pub states: Arc<Mutex<Vec<String>>>,
    pub clock: FakeClock,
    pub inlet: Arc<TelegramInlet>,
}

impl Rig {
    /// `Rig(api, create, **kw)`: `tweak` sets the rest of the keyword arguments.
    pub fn new(api: Arc<FakeApi>, create: Arc<FakeCreate>, config: TelegramConfig, tweak: impl FnOnce(&mut InletOptions)) -> Self {
        let agents = Arc::new(Agents::default());
        let states = Arc::new(Mutex::new(Vec::new()));
        let clock = FakeClock::new(0.0);
        let s = states.clone();
        let mut options = InletOptions {
            agent_factory: Some(agents.factory()),
            brief: Some(Arc::new(String::new)),
            on_state: Some(Arc::new(move |state: &str| s.lock().push(state.to_string()))),
            clock: Some(clock.clock()),
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
        let inlet = TelegramInlet::new(config, api.clone(), create.create(), options);
        Self { api, create, agents, states, clock, inlet }
    }

    /// `Rig(api, create)` over an API that is not a FakeApi (a MutedBotApi): the same lent fakes, and `api` is an
    /// unused FakeApi.
    pub fn over(other: Arc<dyn TelegramApi>, create: Arc<FakeCreate>, config: TelegramConfig) -> Self {
        let made = Self::new(FakeApi::new(vec![]), create.clone(), config.clone(), |_| {});
        let s = made.states.clone();
        let options = InletOptions {
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
        let inlet = TelegramInlet::new(config, other, create.create(), options);
        Self { inlet, ..made }
    }
}

/// conftest.py's autouse pins for every Python test: the menu tools off (`_menu_tools_pinned_off`), Ask science,
/// the NYT tools and pictures for cards off, and the phone's fix and places history in files that do not exist
/// (`_phone_location_in_tmp`); a test that wants one on sets it in its own environ.
pub fn conftest_env() -> HashMap<String, String> {
    [
        ("EVE_MENU_TOOLS", "off"),
        ("EVE_SCIENCE_SEARCH", "0"),
        ("EVE_NYT", "0"),
        ("EVE_FIND_IMAGE", "0"),
        ("EVE_PHONE_LOCATION_FILE", "/nonexistent/eve-brain-tests/phone/phone-location.json"),
        ("EVE_PLACES_FILE", "/nonexistent/eve-brain-tests/phone/places.db"),
    ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

// ---- the voice's fakes --------------------------------------------------------------------------------------------

/// The Live connection, faked: what the session sent, and the events it will read.
pub struct FakeConn {
    pub sent: Mutex<Vec<(String, Value)>>,
    events_tx: tokio::sync::mpsc::UnboundedSender<Value>,
    events_rx: tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<Value>>,
    pub closed: AtomicBool,
}

impl FakeConn {
    pub fn new() -> Arc<Self> {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        Arc::new(Self {
            sent: Mutex::new(Vec::new()),
            events_tx: tx,
            events_rx: tokio::sync::Mutex::new(rx),
            closed: AtomicBool::new(false),
        })
    }

    /// The server sends `event`.
    pub fn push(&self, event: Value) {
        let _ = self.events_tx.send(event);
    }

    /// The server closes the connection: `next_event` then answers None.
    pub fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let _ = self.events_tx.send(Value::Null);
    }

    /// The sends named `kind`, in order.
    pub fn of(&self, kind: &str) -> Vec<Value> {
        self.sent.lock().iter().filter(|(k, _)| k == kind).map(|(_, v)| v.clone()).collect()
    }

    fn record(&self, kind: &str, value: Value) -> anyhow::Result<()> {
        self.sent.lock().push((kind.to_string(), value));
        Ok(())
    }
}

#[async_trait]
impl LiveConnection for FakeConn {
    async fn session_start(&self, session: Value) -> anyhow::Result<()> {
        self.record("session.start", session)
    }
    async fn session_update(&self, session: Value) -> anyhow::Result<()> {
        self.record("session.update", session)
    }
    async fn commentary_append(&self, content: &str, delegation_id: Option<&str>) -> anyhow::Result<()> {
        self.record("commentary.append", json!({"content": content, "delegation_id": delegation_id}))
    }
    async fn thinking_append(&self, content: &str, delegation_id: Option<&str>) -> anyhow::Result<()> {
        self.record("thinking.append", json!({"content": content, "delegation_id": delegation_id}))
    }
    async fn input_audio_append(&self, audio_b64: &str) -> anyhow::Result<()> {
        self.record("input_audio.append", json!(audio_b64))
    }
    async fn response_create(&self) -> anyhow::Result<()> {
        self.record("response.create", Value::Null)
    }
    async fn response_item_create(&self, item: Value) -> anyhow::Result<()> {
        self.record("response.item.create", item)
    }
    async fn next_event(&self) -> Option<anyhow::Result<Value>> {
        if self.closed.load(Ordering::SeqCst) {
            return None;
        }
        let event = self.events_rx.lock().await.recv().await?;
        if event.is_null() {
            return None;
        }
        Some(Ok(event))
    }
}

/// A speaker that records what it was asked to play.
#[derive(Default)]
pub struct FakeSpeaker {
    pub played: Mutex<Vec<Vec<u8>>>,
    pub stops: AtomicI64,
    pub busy: AtomicBool,
    pub closed: AtomicBool,
}

impl VoiceSpeaker for FakeSpeaker {
    fn start(&self) -> bool {
        true
    }
    fn play(&self, pcm: &[u8]) {
        self.played.lock().push(pcm.to_vec());
    }
    fn stop(&self) {
        self.stops.fetch_add(1, Ordering::SeqCst);
    }
    fn busy(&self) -> bool {
        self.busy.load(Ordering::SeqCst)
    }
    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }
}

/// A scratch folder for one test, removed when it is dropped.
pub fn tmp() -> tempfile::TempDir {
    tempfile::tempdir().expect("a temp dir")
}

pub fn path(dir: &tempfile::TempDir, name: &str) -> PathBuf {
    dir.path().join(name)
}

// ---- caplog --------------------------------------------------------------------------------------------------------

/// Log lines written on this thread while it is alive (pytest's caplog at INFO). One global subscriber writes each
/// line to the capturing thread's buffer: a scoped (`set_default`) subscriber loses lines once another test's
/// callsite caches "nobody listens" for the whole process. A current-thread test runtime runs every task here.
pub struct Caplog {
    buf: Arc<Mutex<Vec<u8>>>,
}

thread_local! {
    static CAPLOG: std::cell::RefCell<Option<Arc<Mutex<Vec<u8>>>>> = const { std::cell::RefCell::new(None) };
}

struct CaplogWriter;

impl std::io::Write for CaplogWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        CAPLOG.with(|c| {
            if let Some(buf) = &*c.borrow() {
                buf.lock().extend_from_slice(data);
            }
        });
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Caplog {
    pub fn capture() -> Self {
        static INSTALL: std::sync::Once = std::sync::Once::new();
        INSTALL.call_once(|| {
            let subscriber = tracing_subscriber::fmt()
                .with_max_level(tracing::Level::INFO)
                .with_ansi(false)
                .with_writer(|| CaplogWriter)
                .finish();
            let _ = tracing::subscriber::set_global_default(subscriber);
        });
        let buf = Arc::new(Mutex::new(Vec::new()));
        CAPLOG.with(|c| *c.borrow_mut() = Some(buf.clone()));
        Self { buf }
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.buf.lock()).into_owned()
    }
}

impl Drop for Caplog {
    fn drop(&mut self) {
        CAPLOG.with(|c| *c.borrow_mut() = None);
    }
}
