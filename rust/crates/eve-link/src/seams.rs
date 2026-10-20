//! What the Python modules reached by duck typing on other modules' objects, as traits.
//!
//! * [`Inlet`]: the Telegram chat's brain (telegram.TelegramInlet), read on every request through an [`InletFn`].
//! * [`Chief`]: the chief of staff (chief.Chief) as link_chat, link_today and updates read its cards and call its
//!   own methods; a card is a [`JobCard`] view taken when it is read.
//! * [`MiniApp`] and [`Hub`]: the Mini App server (miniapp.MiniAppServer) and its cards (lights, Spotify, the TV,
//!   Pika, the OBSBOT camera, eve's Memory and Notes), each with `panel(body)`.
//! * [`DirectApps`]: the direct Google apps (apps_mcp.AppsBridge): `apps` and `execute`.
//! * [`ComposioRead`]: the Composio bridge's reads for Updates (composio_tools.ComposioBridge).
//! * [`WorkDoor`]: the Work tab's Claude Code sessions (work_sessions.WorkSessions).
//!
//! The production objects this crate can reach implement them here (AppsBridge, ComposioBridge, WorkSessions); the
//! brain and the daemon implement [`Inlet`], [`Chief`], [`MiniApp`] and [`Hub`] for theirs.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Map, Value};
use tokio::io::{AsyncRead, AsyncWrite};

use eve_composio_tools::ComposioError;
use eve_link_reactions::Reactions;
use eve_link_uploads::{Upload, UploadStore};
use eve_transcripts::Transcripts;
use eve_web::watch::Watch;

/// A request's headers as miniapp.py parsed them: names lowercased.
pub type Headers = HashMap<String, String>;

/// The request's body, after its head (Python's `asyncio.StreamReader`).
pub type DynRead = dyn AsyncRead + Unpin + Send;

/// The connection's write half (Python's `asyncio.StreamWriter`).
pub type DynWrite = dyn AsyncWrite + Unpin + Send;

/// A JSON object, as Python's dicts are passed.
pub type Obj = Map<String, Value>;

// ---- the chat's brain ---------------------------------------------------------------------------------------

/// `TelegramInlet.hear(text, on_call=..., app=..., grade=..., attachments=...)`'s keywords.
#[derive(Debug, Clone)]
pub struct HearOptions {
    /// a call's words (false: typed)
    pub on_call: bool,
    /// the app turn's id (`app=<turn id>`, the inlet's `_FROM_APP`); None: not the app's
    pub app: Option<String>,
    /// telegram.GRADES: "auto", "instant" or "think"
    pub grade: String,
    /// the turn's uploads (link_uploads.Upload); passed only when there are some
    pub attachments: Vec<Upload>,
}

/// `TelegramInlet.app_out`: one of eve's lines from app turn `turn_id` → whether a stream is reading it. Called as
/// `(turn_id, text, kind)`, kind "say" or "ask" (an "ask" ends the turn's stream after it). False: the inlet sends
/// the line to Telegram.
pub type AppOut = Arc<dyn Fn(&str, &str, &str) -> bool + Send + Sync>;

/// The Telegram chat's brain (telegram.TelegramInlet) as this crate uses it. Every method with a default stands for
/// a Python `getattr(inlet, name, None)` whose None the callers handle: the default is what None meant.
#[async_trait]
pub trait Inlet: Send + Sync {
    /// `inlet._chat_id is not None`: the chat runs and the owner has texted it.
    fn chat_ready(&self) -> bool;

    /// Hear the owner's words as typed in the Mac chat window. It schedules the turn and returns; an error is a turn
    /// that could not start.
    fn hear(&self, text: &str, opts: HearOptions) -> anyhow::Result<()>;

    /// A turn the owner started is in flight (`owner_busy()`; an inlet without it: never).
    fn owner_busy(&self) -> bool {
        false
    }

    /// The turn in flight only waits on the owner's answer to a question (`waiting_on_owner()`).
    fn waiting_on_owner(&self) -> bool {
        false
    }

    /// `app_filed(turn_id)`: the card id that app turn's own take_on filed, if any.
    fn app_filed(&self, _turn_id: &str) -> Option<String> {
        None
    }

    /// `inlet.app_out = door.deliver`.
    fn set_app_out(&self, out: AppOut);

    /// `inlet.link_uploads = door.uploads`: eve's own pictures and files land in the door's store. An inlet without
    /// the attribute ignores it.
    fn set_link_uploads(&self, _uploads: Arc<UploadStore>) {}

    /// `inlet.transcripts`: the transcript store (its `enabled` says whether memory is on).
    fn transcripts(&self) -> Option<Arc<Transcripts>> {
        None
    }

    /// `inlet.app_reactions`: the app's reactions (link_reactions.Reactions).
    fn app_reactions(&self) -> Option<Arc<Reactions>> {
        None
    }

    /// `inlet.chief`: the chief of staff, or None while EVE_CHIEF is off.
    fn chief(&self) -> Option<Arc<dyn Chief>> {
        None
    }

    /// `inlet._watcher.watches`, read now: None when there is no watcher.
    fn watches(&self) -> Option<Vec<Watch>> {
        None
    }

    /// `inlet._direct`: the direct Google apps (apps_mcp.AppsBridge), or None.
    fn direct_apps(&self) -> Option<Arc<dyn DirectApps>> {
        None
    }

    /// `Path(inlet._vault.root)`: the second brain's vault the chat uses; None when the chat has none (updates then
    /// reads second_brain.configured()).
    fn vault_root(&self) -> Option<PathBuf> {
        None
    }

    /// `inlet._apps`: the owner's Composio bridge, or None.
    fn composio(&self) -> Option<Arc<dyn ComposioRead>> {
        None
    }

    /// `inlet.muse`: the composer's eve/Muse switch is on Muse.
    fn muse(&self) -> bool {
        false
    }

    /// `inlet.set_muse(on, tell=tell)`.
    fn set_muse(&self, _on: bool, _tell: bool) {}
}

/// Returns the chat's brain now, or None (read on every request, so a chat that starts later is found).
pub type InletFn = Arc<dyn Fn() -> Option<Arc<dyn Inlet>> + Send + Sync>;

// ---- the chief's cards ----------------------------------------------------------------------------------------

/// One step of a card (chief_card.Phase), as the Work view reads it.
#[derive(Debug, Clone, PartialEq)]
pub struct JobPhase {
    pub n: i64,
    /// "queued", "running", "ok", ...
    pub status: String,
    /// "one_way" or "two_way"
    pub door: String,
    /// the owner's yes on it (`phase.approval`), None until given
    pub approval: Option<Value>,
    /// what the step does: "research", "assess", "act", "watch"
    pub do_: String,
}

/// One card (chief_card.Card) as link_chat reads it, with what the chief says of it now.
#[derive(Debug, Clone, PartialEq)]
pub struct JobCard {
    pub id: String,
    /// "task", "commitment", ...
    pub kind: String,
    /// "proposed", "waiting", "running", "unverified", "done", "dropped", "failed", ...
    pub status: String,
    /// "go", "budget", "question", "mac_slot", or None
    pub waiting_for: Option<String>,
    /// `card.terminal`
    pub terminal: bool,
    pub rev: i64,
    pub updated: f64,
    pub phases: Vec<JobPhase>,
    /// `card.cue.at` when the card has a cue with a time
    pub cue_at: Option<String>,
    /// `chief._doubtful(card) is not None`: its act may already have happened
    pub doubtful: bool,
    /// `chief._as_data(card)`, in its key order
    pub data: Obj,
}

/// The chief of staff (chief.Chief): its ledger's cards and its own methods, as the Telegram code words call them.
#[async_trait]
pub trait Chief: Send + Sync {
    /// `chief.ledger.all()`, as views taken now.
    fn cards(&self) -> Vec<JobCard>;
    /// `chief.ledger.get(cid)`.
    fn card(&self, cid: &str) -> Option<JobCard>;
    /// `chief.approve(cid, via=via, rev=rev)`: the owner's yes.
    async fn approve(&self, cid: &str, via: &str, rev: Option<i64>) -> anyhow::Result<Obj>;
    /// `await chief.done(cid)`.
    async fn done(&self, cid: &str) -> anyhow::Result<Obj>;
    /// `chief.drop(cid)`.
    async fn drop_job(&self, cid: &str) -> anyhow::Result<Obj>;
    /// `await chief.happened(cid)`.
    async fn happened(&self, cid: &str) -> anyhow::Result<Obj>;
    /// `chief.didnt(cid)`.
    async fn didnt(&self, cid: &str) -> anyhow::Result<Obj>;
    /// `chief.raise_budget(cid)`.
    async fn raise_budget(&self, cid: &str) -> anyhow::Result<Obj>;
}

// ---- the Mini App server and its cards ------------------------------------------------------------------------

/// One of the Mini App server's cards (lights.Lights, spotify.Spotify, firetv.FireTV, pika, obsbot_panel, eve's
/// Memory and Notes panels): `available`, `listing()`, `panel(body)` and, for an app with a picture, `stream`.
#[async_trait]
pub trait Hub: Send + Sync {
    /// `getattr(hub, "available", True)`.
    fn available(&self) -> bool {
        true
    }
    /// `hub.listing()`: the lights' names and rooms (only an app whose registry entry has `listing`).
    fn listing(&self) -> Vec<Value> {
        Vec::new()
    }
    /// `await hub.panel(body)`: the card's own answer, its JSON as it is. An error is the card failing.
    async fn panel(&self, body: &Value) -> anyhow::Result<Value>;
    /// `await hub.stream(writer)`: the picture as multipart JPEGs, the whole HTTP answer written here. Only an app
    /// whose registry entry has `stream` is asked.
    async fn stream(&self, _writer: &mut DynWrite) -> std::io::Result<()> {
        Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "this card has no picture"))
    }
}

/// An app the owner built (apps_maker.AppInfo), as the app's list shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct AppInfo {
    pub slug: String,
    pub title: String,
    pub updated: f64,
}

/// The owner's built apps (apps_maker.AppStore): `list()` (last changed first) and `info(slug)`.
pub trait AppStoreView: Send + Sync {
    fn list(&self) -> Vec<AppInfo>;
    fn info(&self, slug: &str) -> Option<AppInfo>;
}

/// The Mini App server (miniapp.MiniAppServer) as the door reads it on every request.
pub trait MiniApp: Send + Sync {
    /// `getattr(server, name, None)`: the card named `name` (an app's slot, "memory", "notes"), or None.
    fn hub(&self, name: &str) -> Option<Arc<dyn Hub>>;
    /// `server.store`: the app builder's store, None while it is off.
    fn app_store(&self) -> Option<Arc<dyn AppStoreView>>;
    /// `server.app_link(slug, headers)`: the app's page at the tunnel's origin with a fresh app token.
    fn app_link(&self, slug: &str, headers: &Headers) -> String;
}

// ---- the direct Google apps -------------------------------------------------------------------------------------

/// apps_mcp.AppsBridge as link_today and updates call it.
#[async_trait]
pub trait DirectApps: Send + Sync {
    /// `apps.apps`: the apps signed in ("gmail", "calendar", ...).
    fn apps(&self) -> Vec<String>;
    /// `apps.execute(name, args)`: `{"ok": false, "reason"}` on a failure, else the answer with its `result`.
    async fn execute(&self, name: &str, args: &Obj) -> Obj;
}

#[async_trait]
impl DirectApps for eve_apps_mcp::AppsBridge {
    fn apps(&self) -> Vec<String> {
        eve_apps_mcp::AppsBridge::apps(self)
    }

    async fn execute(&self, name: &str, args: &Obj) -> Obj {
        eve_apps_mcp::AppsBridge::execute(self, name, args).await
    }
}

// ---- Composio ----------------------------------------------------------------------------------------------------

/// The Composio bridge's reads for Updates (composio_tools.ComposioBridge: `can_read`, `connected_accounts`,
/// `read_tool`).
#[async_trait]
pub trait ComposioRead: Send + Sync {
    /// The client is up, so the reads can run.
    fn can_read(&self) -> bool;
    /// The owner's connected accounts, each `{"toolkit", "id", "alias", "status"}`.
    async fn connected_accounts(&self) -> Result<Vec<Value>, ComposioError>;
    /// ONE app tool that only reads, on account `account_id`: `{"ok": false, "reason"}` or `{"ok": true, "data"}`.
    async fn read_tool(&self, slug: &str, arguments: &Obj, account_id: &str) -> Value;
}

#[async_trait]
impl ComposioRead for eve_composio_tools::ComposioBridge {
    fn can_read(&self) -> bool {
        eve_composio_tools::ComposioBridge::can_read(self)
    }

    async fn connected_accounts(&self) -> Result<Vec<Value>, ComposioError> {
        let got = eve_composio_tools::ComposioBridge::connected_accounts(self).await?;
        Ok(got.iter().map(|a| a.to_value()).collect())
    }

    async fn read_tool(&self, slug: &str, arguments: &Obj, account_id: &str) -> Value {
        Value::Object(eve_composio_tools::ComposioBridge::read_tool(self, slug, arguments, account_id).await)
    }
}

// ---- Work sessions -------------------------------------------------------------------------------------------------

/// work_sessions.WorkSessions as the door calls it: each answer is `(status, body)`.
#[async_trait]
pub trait WorkDoor: Send + Sync {
    async fn list(&self) -> (u16, Value);
    async fn start(&self, task: &Value, client_id: &Value) -> (u16, Value);
    async fn detail(&self, sid: &str) -> (u16, Value);
    async fn reply(&self, sid: &str, text: &Value) -> (u16, Value);
    async fn stop(&self, sid: &str) -> std::io::Result<(u16, Value)>;
}

#[async_trait]
impl WorkDoor for eve_work_sessions::WorkSessions {
    async fn list(&self) -> (u16, Value) {
        eve_work_sessions::WorkSessions::list(self).await
    }

    async fn start(&self, task: &Value, client_id: &Value) -> (u16, Value) {
        eve_work_sessions::WorkSessions::start(self, task, client_id).await
    }

    async fn detail(&self, sid: &str) -> (u16, Value) {
        eve_work_sessions::WorkSessions::detail(self, sid).await
    }

    async fn reply(&self, sid: &str, text: &Value) -> (u16, Value) {
        eve_work_sessions::WorkSessions::reply(self, sid, text).await
    }

    async fn stop(&self, sid: &str) -> std::io::Result<(u16, Value)> {
        eve_work_sessions::WorkSessions::stop(self, sid).await
    }
}

/// Returns the Work sessions now, or None (`EVE_WORK_SESSIONS=0`: the routes are a 503).
pub type WorkFn = Arc<dyn Fn() -> Option<Arc<dyn WorkDoor>> + Send + Sync>;
