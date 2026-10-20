//! The app's chat door (`link_chat.LinkChat`): every `/api/link/*` request after miniapp.py read its head.
//!
//! **Who may call.** The link token (stick_link.LinkToken), as `Authorization: Bearer`, checked in constant time by
//! the caller's `token_check`. A browser sends an Origin on every cross-site fetch and the app's URLSession sends
//! none, so any Origin is refused (403) before the token is even read.
//!
//! **Where the answer goes.** Everything an app turn makes eve say goes to the app's open stream ([`LinkChat::deliver`],
//! the inlet's `app_out`) and is not posted to Telegram; when no stream is reading, the line goes to Telegram as
//! before. See `link_chat.py`'s docstring for the whole contract.
//!
//! Python's private methods map one to one: `_attach`, `_on_append`/`_wake`, `_newest_open`, `_prune`, `_turn`,
//! `_start`, `_drive`, `_stream`, `_topics`, `_messages`, `_older`, `_thumbs`, `_upload`, `_file`, `_jobs`,
//! `_new_job`, `_await_card`, `_optional_body`, `_updates`, `_location`, `_places`, `_sessions`, `_apps`,
//! `_controls`, `_muse`, `_busy`, `_discard`, `_json`.

use std::collections::HashMap;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;

use eve_link_uploads::UploadStore;
use eve_transcripts::Transcripts;

use crate::link_chat::{
    HiddenJobs, CLOSE_SETTLE_SECS, JOB_SECS, PING_SECS, POLL_SECS, SETTLE_SECS, TURN_SECS,
};
use crate::link_today::{Today, TodayOptions};
use crate::link_topics::{Bookmarks, Titler};
use crate::seams::{AppOut, DynRead, DynWrite, Headers, InletFn, MiniApp, WorkFn};
use crate::updates::{Updates, UpdatesOptions};
use crate::{Env, Mono};

/// What `LinkChat::new` takes beside the inlet; `Default` is production (every store at its default path).
#[derive(Clone)]
pub struct LinkChatOptions {
    /// monotonic seconds (`time.monotonic`; tests pass a fake)
    pub clock: Option<Mono>,
    pub turn_secs: f64,
    pub ping_secs: f64,
    pub settle_secs: f64,
    pub poll_secs: f64,
    pub job_secs: f64,
    /// link_uploads.UploadStore
    pub uploads: Option<Arc<UploadStore>>,
    /// link_today.Today: the widget's feed
    pub today: Option<Arc<Today>>,
    /// finished cards removed from Work
    pub hidden: Option<Arc<HiddenJobs>>,
    /// updates.Updates: the Updates pane's cards; the daemon runs its refresh loop (daemon.Daemon.run)
    pub updates: Option<Arc<Updates>>,
    /// work_sessions.WorkSessions, read per request; None: the routes are a 503
    pub work: Option<WorkFn>,
    /// Horizontal History's saved topics
    pub bookmarks: Option<Arc<Bookmarks>>,
    /// and their titles, from a cheap model
    pub titler: Option<Arc<Titler>>,
    /// the environment phone_location and places read (None: the process's)
    pub environ: Option<Env>,
}

impl Default for LinkChatOptions {
    fn default() -> Self {
        LinkChatOptions {
            clock: None,
            turn_secs: TURN_SECS,
            ping_secs: PING_SECS,
            settle_secs: SETTLE_SECS,
            poll_secs: POLL_SECS,
            job_secs: JOB_SECS,
            uploads: None,
            today: None,
            hidden: None,
            updates: None,
            work: None,
            bookmarks: None,
            titler: None,
            environ: None,
        }
    }
}

/// One app turn: eve's lines for it, in order, and whether it is over. `readers`: streams writing it now (two when
/// the phone retried with the same client_id); a line nobody reads goes to Telegram (`deliver`). `asked`: the turn
/// asked the owner something and waits; the owner's answer comes as a new turn, whose stream carries the rest of
/// this one's reply. Python's `_Turn`.
pub(crate) struct Turn {
    pub(crate) id: String,
    pub(crate) client_id: String,
    pub(crate) born: f64,
    pub(crate) state: parking_lot::Mutex<TurnState>,
    /// `poke()`: bumped on every change, so no waiter misses one (Python's fresh asyncio.Event each time)
    pub(crate) changed: tokio::sync::watch::Sender<u64>,
}

#[derive(Debug, Default)]
pub(crate) struct TurnState {
    pub(crate) chunks: Vec<String>,
    pub(crate) done: bool,
    pub(crate) asked: bool,
    pub(crate) readers: i64,
}

/// The door's own state: the turns kept for dedupe (by turn id, oldest first), by client id, the New jobs by
/// client id, the turn drivers, and the transcript store whose listeners already hold this door's waker.
#[derive(Default)]
pub(crate) struct DoorState {
    pub(crate) turns: IndexMap<String, Arc<Turn>>,
    pub(crate) by_client: HashMap<String, Arc<Turn>>,
    pub(crate) job_starts: HashMap<String, Arc<Turn>>,
    pub(crate) drivers: Vec<tokio::task::JoinHandle<()>>,
    pub(crate) streams: usize,
    pub(crate) store: Option<Arc<Transcripts>>,
}

pub(crate) struct Shared {
    pub(crate) inlet: InletFn,
    pub(crate) opts: LinkChatOptions,
    pub(crate) clock: Mono,
    pub(crate) today: Arc<Today>,
    pub(crate) updates: Arc<Updates>,
    pub(crate) bookmarks: Arc<Bookmarks>,
    pub(crate) titler: Arc<Titler>,
    pub(crate) uploads: Arc<UploadStore>,
    pub(crate) hidden: Arc<HiddenJobs>,
    pub(crate) state: parking_lot::Mutex<DoorState>,
    /// set (bumped) when a transcript line is written or a reaction changes: wakes the long polls, from any thread
    pub(crate) appended: tokio::sync::watch::Sender<u64>,
    /// bumped when an open stream ends (`settle` waits on it)
    pub(crate) streams_changed: tokio::sync::watch::Sender<u64>,
}

/// The app's chat door. `inlet` returns the Telegram chat's brain or None; it is read on every request, so a chat
/// that starts after the Mini App is found. The inlet's `app_out` is pointed at [`LinkChat::deliver`] and its
/// transcript store's listeners at this door's wake-up on the first request. A cheap handle: clones share one door.
#[derive(Clone)]
pub struct LinkChat {
    pub(crate) inner: Arc<Shared>,
}

impl LinkChat {
    /// The door, its stores made at their default paths unless `opts` lends them (`Today::new`, `Updates::new` with
    /// this inlet, `Bookmarks::new(None)`, `Titler::new(None, None, None)`, `UploadStore::default()`,
    /// `HiddenJobs::new(None)`).
    pub fn new(inlet: InletFn, opts: LinkChatOptions) -> Self {
        let _ = (inlet, opts, TodayOptions::default(), UpdatesOptions::default());
        todo!()
    }

    pub fn today(&self) -> &Arc<Today> {
        &self.inner.today
    }

    pub fn updates(&self) -> &Arc<Updates> {
        &self.inner.updates
    }

    pub fn bookmarks(&self) -> &Arc<Bookmarks> {
        &self.inner.bookmarks
    }

    pub fn titler(&self) -> &Arc<Titler> {
        &self.inner.titler
    }

    pub fn uploads(&self) -> &Arc<UploadStore> {
        &self.inner.uploads
    }

    pub fn hidden(&self) -> &Arc<HiddenJobs> {
        &self.inner.hidden
    }

    /// `deliver` as the inlet's `app_out` (it holds the door weakly).
    pub fn app_out(&self) -> AppOut {
        todo!()
    }

    /// One of eve's lines from work app turn `turn_id` began (TelegramInlet.app_out) → whether a stream is reading
    /// it. Its own turn's stream while that is open; once it ended on a question, the newest open stream (the owner's
    /// answer, sent from the app, whose stream carries the rest of the reply). False: no stream will show it, so the
    /// inlet sends it to Telegram. An "ask" ends the turn's stream after it.
    pub fn deliver(&self, turn_id: &str, text: &str, kind: &str) -> bool {
        let _ = (turn_id, text, kind);
        todo!()
    }

    /// The server is stopping: every turn ends, and its streams with it.
    pub fn close(&self) {
        todo!()
    }

    /// After `close`: let the open streams write their [DONE] and the 0 chunk, for at most `timeout` seconds
    /// (`CLOSE_SETTLE_SECS`), before the server cancels their connections. Without it the cancel landed first and
    /// the app saw a cut stream on every daemon restart (review, 2026-10-08).
    pub async fn settle(&self, timeout: f64) {
        let _ = (timeout, CLOSE_SETTLE_SECS);
        todo!()
    }

    /// One request on `/api/link/*`, its head already read by miniapp.MiniAppServer. `query` is the part after `?`
    /// (no `?`); `headers` lowercased. `token_check` is the link token's (stick_link.LinkToken.check), or None before
    /// the phone was ever paired. `apps`: the Mini App's server (its store, app_link and cards), or None. The whole
    /// answer is written to `writer` (status line, headers, body; `Connection: close`).
    #[allow(clippy::too_many_arguments)]
    pub async fn serve(
        &self,
        method: &str,
        path: &str,
        query: &str,
        headers: &Headers,
        reader: &mut DynRead,
        writer: &mut DynWrite,
        token_check: Option<&(dyn Fn(&str) -> bool + Send + Sync)>,
        apps: Option<Arc<dyn MiniApp>>,
    ) -> std::io::Result<()> {
        let _ = (method, path, query, headers, reader, writer, token_check, apps);
        todo!()
    }

    /// The payload's `busy`: a turn the owner started is in flight and is not just waiting on the owner's answer to
    /// a question (no "replying" dots under eve's own question).
    pub fn busy(inlet: &dyn crate::seams::Inlet) -> bool {
        let _ = inlet;
        todo!()
    }

    /// `_json`: `HTTP/1.1 <status> <reason>`, `Content-Type: application/json`, `Content-Length`,
    /// `Cache-Control: no-store`, `Connection: close`, and the body as Python's `json.dumps` writes it.
    pub async fn write_json(writer: &mut DynWrite, status: u16, obj: &Value) -> std::io::Result<()> {
        let _ = (writer, status, obj);
        todo!()
    }

    /// Read and drop a refused body (at most DISCARD_MAX_BYTES, for DISCARD_SECS) before the refusal is written.
    /// Answering and closing while the client still sends made the client see "connection reset" instead of the 413
    /// (a test caught it about once in 35 runs, 2026-10-08). A larger body is still cut off.
    pub async fn discard(reader: &mut DynRead, size: i64) {
        let _ = (reader, size);
        todo!()
    }
}
