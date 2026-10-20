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
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Weak};
use std::time::Duration;

use futures::FutureExt;
use indexmap::IndexMap;
use serde_json::{json, Map, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::Instant;

use eve_link_uploads::UploadStore;
use eve_places::{phone_location, places};
use eve_transcripts::py as tpy;
use eve_transcripts::Transcripts;

use crate::link_chat::{
    chunk, controls_listing, job_entry, length, list_jobs, py_json, read_before, read_messages, reason, split_cursor,
    sse, strip_openui, turn_id, HiddenJobs, APPS_PATH, APP_OPEN_RE, BODY_SECS, CONTROLS,
    CONTROLS_PATH, CONTROL_RE, DEDUPE_SECS, DISCARD_MAX_BYTES, DISCARD_SECS, DONE, FILE_RE, GRADES, HISTORY_LIMIT,
    INLINE_KINDS, JOBS_PATH, JOB_ACTIONS, JOB_ACTION_RE, JOB_DETAIL_LEAD, JOB_GRACE_SECS, JOB_LEAD, JOB_SECS,
    LOCATION_PATH, MAX_ATTACHMENTS, MAX_BODY_BYTES, MAX_CARD_ID_CHARS, MAX_CLIENT_ID_CHARS, MAX_DETAIL_CHARS,
    MAX_GOAL_CHARS, MAX_INLINE_BYTES, MAX_KEPT_TURNS, MAX_TEXT_CHARS, MUSE_PATH, NO_APPS, NO_CHAT, NO_CHIEF,
    NO_CONTROL, NO_LOCATION, NO_MEMORY, NO_PLACES, NO_WORK, PATHS, PING, PING_SECS, PLACES_FORGET_PATH, PLACES_PATH,
    POLL_SECS, POST_PATHS, SESSIONS_PATH, SESSION_RE, SESSION_START_PATH, SETTLE_SECS, STREAM_HEAD, STREAM_PATHS,
    TODAY_PATH, TOPICS_BOOKMARK_PATH, TOPICS_PATH, TURN_PATH, TURN_SECS, UPDATES_DISMISS_PATH, UPDATES_PATH,
    UPDATES_REFRESH_PATH, UPLOAD_PATH, UPLOAD_SECS, VISIT_PATH, WAIT_MAX_SECS,
};
use crate::link_today::{Today, TodayOptions};
use crate::link_topics::{Bookmarks, Titler};
use crate::seams::{AppOut, Chief, DynRead, DynWrite, Headers, HearOptions, Inlet, InletFn, MiniApp, Obj, WorkFn};
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

impl Turn {
    fn new(id: String, client_id: String, born: f64) -> Arc<Self> {
        Arc::new(Turn {
            id,
            client_id,
            born,
            state: parking_lot::Mutex::new(TurnState::default()),
            changed: tokio::sync::watch::channel(0).0,
        })
    }

    /// Wake every stream writing this turn.
    fn poke(&self) {
        self.changed.send_modify(|n| *n = n.wrapping_add(1));
    }

    fn done(&self) -> bool {
        self.state.lock().done
    }

    fn add_reader(&self, n: i64) {
        self.state.lock().readers += n;
    }
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
    /// `_on_append` as the reactions hold it: one object, so it is added to a store's listeners only once
    pub(crate) reaction_waker: eve_link_reactions::Listener,
}

impl Shared {
    /// `_wake`: every long poll re-reads now. Safe from any thread (Python's `_on_append` hops to the loop).
    fn wake(&self) {
        self.appended.send_modify(|n| *n = n.wrapping_add(1));
    }

    fn clock(&self) -> f64 {
        (self.clock)()
    }
}

/// The app's chat door. `inlet` returns the Telegram chat's brain or None; it is read on every request, so a chat
/// that starts after the Mini App is found. The inlet's `app_out` is pointed at [`LinkChat::deliver`] and its
/// transcript store's listeners at this door's wake-up on the first request. A cheap handle: clones share one door.
#[derive(Clone)]
pub struct LinkChat {
    pub(crate) inner: Arc<Shared>,
}

/// Seconds as a Duration for a wait: a negative or broken figure is no wait, a huge one the longest.
fn secs(s: f64) -> Duration {
    if s.is_nan() || s <= 0.0 {
        Duration::ZERO
    } else {
        Duration::try_from_secs_f64(s).unwrap_or(Duration::MAX)
    }
}

/// Python's `len(s)`: code points.
fn chars(s: &str) -> usize {
    s.chars().count()
}

/// `urllib.parse.unquote` (UTF-8, errors replaced); with `plus`, `unquote_plus`.
fn unquote(raw: &str, plus: bool) -> String {
    let raw = if plus { raw.replace('+', " ") } else { raw.to_string() };
    String::from_utf8_lossy(&urlencoding::decode_binary(raw.as_bytes())).into_owned()
}

/// `dict(parse_qsl(query, keep_blank_values=True))`: a later key wins.
fn parse_qsl(query: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for field in query.split('&') {
        if field.is_empty() {
            continue;
        }
        let (k, v) = field.split_once('=').unwrap_or((field, ""));
        out.insert(unquote(k, true), unquote(v, true));
    }
    out
}

/// Python's `int(text)`: whitespace around, a sign, digits with single underscores between them. A value past i64
/// saturates (it is clamped by the caller anyway).
fn py_int(text: &str) -> Option<i64> {
    let s = tpy::strip(text);
    let (neg, digits) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if digits.is_empty() || digits.starts_with('_') || digits.ends_with('_') || digits.contains("__") {
        return None;
    }
    let mut n: i64 = 0;
    for c in digits.chars() {
        if c == '_' {
            continue;
        }
        // any Unicode decimal digit counts, as in Python's int()
        let d = tpy::int(c.encode_utf8(&mut [0u8; 4]))?;
        n = n.saturating_mul(10).saturating_add(d);
    }
    Some(if neg { -n } else { n })
}

/// Python's `float(text)`: whitespace around, underscores between digits, `inf` and `nan` in any case.
fn py_float(text: &str) -> Option<f64> {
    let s = tpy::strip(text);
    if s.contains('_') {
        let b: Vec<char> = s.chars().collect();
        for (i, c) in b.iter().enumerate() {
            if *c == '_' {
                let before = i.checked_sub(1).and_then(|j| b.get(j)).is_some_and(|c| c.is_ascii_digit());
                let after = b.get(i + 1).is_some_and(|c| c.is_ascii_digit());
                if !before || !after {
                    return None;
                }
            }
        }
    }
    s.replace('_', "").parse::<f64>().ok()
}

/// A request's JSON body, as `json.loads` reads it, or None.
fn loads(data: &[u8]) -> Option<Value> {
    serde_json::from_slice(data).ok()
}

/// `body.get(key)` on a JSON object, None (Python's) when absent.
fn get<'a>(body: &'a Obj, key: &str) -> Option<&'a Value> {
    body.get(key)
}

async fn read_exactly(reader: &mut DynRead, size: usize, within: f64) -> Option<Vec<u8>> {
    let mut buf = vec![0u8; size];
    match tokio::time::timeout(secs(within), reader.read_exact(&mut buf)).await {
        Ok(Ok(_)) => Some(buf),
        _ => None,
    }
}

/// Run a blocking store read off the event loop (`asyncio.to_thread`).
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> T {
    match tokio::task::spawn_blocking(f).await {
        Ok(v) => v,
        Err(e) => std::panic::resume_unwind(e.into_panic()),
    }
}

/// The end of `_drive`'s `finally`: the turn is over, its streams and the long polls see it now. A drop guard, so a
/// driver cancelled by `close` ends its turn too.
struct DriveEnd {
    shared: Weak<Shared>,
    turn: Arc<Turn>,
}

impl Drop for DriveEnd {
    fn drop(&mut self) {
        self.turn.state.lock().done = true;
        self.turn.poke();
        if let Some(shared) = self.shared.upgrade() {
            shared.wake();
        }
    }
}

/// The end of `_stream`'s `finally`: this request no longer reads the turn (unless its hang-up already said so),
/// and it is no longer an open stream.
struct StreamEnd {
    shared: Arc<Shared>,
    turn: Arc<Turn>,
    reading: bool,
}

impl Drop for StreamEnd {
    fn drop(&mut self) {
        if self.reading {
            self.turn.add_reader(-1);
        }
        {
            let mut state = self.shared.state.lock();
            state.streams = state.streams.saturating_sub(1);
        }
        self.shared.streams_changed.send_modify(|n| *n = n.wrapping_add(1));
    }
}

/// A New job's request stops reading its turn however it ends (`finally: turn.readers -= 1`).
struct ReaderGuard(Arc<Turn>);

impl Drop for ReaderGuard {
    fn drop(&mut self) {
        self.0.add_reader(-1);
    }
}

impl LinkChat {
    /// The door, its stores made at their default paths unless `opts` lends them (`Today::new`, `Updates::new` with
    /// this inlet, `Bookmarks::new(None)`, `Titler::new(None, None, None)`, `UploadStore::default()`,
    /// `HiddenJobs::new(None)`).
    pub fn new(inlet: InletFn, opts: LinkChatOptions) -> Self {
        let today = opts.today.clone().unwrap_or_else(|| Arc::new(Today::new(TodayOptions::default())));
        let updates = opts.updates.clone().unwrap_or_else(|| {
            Arc::new(Updates::new(UpdatesOptions { inlet: Some(Arc::clone(&inlet)), ..UpdatesOptions::default() }))
        });
        let bookmarks = opts.bookmarks.clone().unwrap_or_else(|| Arc::new(Bookmarks::new(None)));
        let titler = opts.titler.clone().unwrap_or_else(|| Arc::new(Titler::new(None, None, None)));
        let uploads = opts.uploads.clone().unwrap_or_else(|| Arc::new(UploadStore::default()));
        let hidden = opts.hidden.clone().unwrap_or_else(|| Arc::new(HiddenJobs::new(None)));
        let clock: Mono = opts.clock.clone().unwrap_or_else(|| Arc::new(crate::monotonic));
        let inner = Arc::new_cyclic(|weak: &Weak<Shared>| {
            let waker = weak.clone();
            let reaction_waker: eve_link_reactions::Listener = Arc::new(move |_key: &str| {
                if let Some(shared) = waker.upgrade() {
                    shared.wake();
                }
                Ok(())
            });
            Shared {
                inlet,
                opts,
                clock,
                today,
                updates,
                bookmarks,
                titler,
                uploads,
                hidden,
                state: parking_lot::Mutex::new(DoorState::default()),
                appended: tokio::sync::watch::channel(0).0,
                streams_changed: tokio::sync::watch::channel(0).0,
                reaction_waker,
            }
        });
        LinkChat { inner }
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
        let weak = Arc::downgrade(&self.inner);
        Arc::new(move |turn_id: &str, text: &str, kind: &str| match weak.upgrade() {
            Some(inner) => LinkChat { inner }.deliver(turn_id, text, kind),
            None => false,
        })
    }

    // -- wiring --

    /// `_attach`: the inlet's `app_out` points at this door, eve's own pictures land in its store, and its transcript
    /// store and reactions wake the long polls.
    fn attach(&self, inlet: &Arc<dyn Inlet>) {
        let shared = &self.inner;
        inlet.set_app_out(self.app_out());
        inlet.set_link_uploads(Arc::clone(&shared.uploads)); // eve's own pictures and files land in this door's store
        if let Some(store) = inlet.transcripts() {
            let fresh = {
                let mut state = shared.state.lock();
                let fresh = !state.store.as_ref().is_some_and(|s| Arc::ptr_eq(s, &store));
                if fresh {
                    state.store = Some(Arc::clone(&store));
                }
                fresh
            };
            if fresh {
                let waker = Arc::downgrade(shared);
                store.add_listener(Arc::new(move |_day: &str| {
                    if let Some(shared) = waker.upgrade() {
                        shared.wake();
                    }
                }));
            }
        }
        if let Some(reactions) = inlet.app_reactions() {
            if !reactions.has_listener(&shared.reaction_waker) {
                // a reaction wakes the long polls as a line does
                reactions.add_listener(Arc::clone(&shared.reaction_waker));
            }
        }
    }

    /// The server is stopping: every turn ends, and its streams with it.
    pub fn close(&self) {
        let (drivers, turns): (Vec<_>, Vec<_>) = {
            let mut state = self.inner.state.lock();
            (state.drivers.drain(..).collect(), state.turns.values().cloned().collect())
        };
        for task in drivers {
            task.abort();
        }
        for turn in turns {
            let ended = {
                let mut t = turn.state.lock();
                let was = t.done;
                t.done = true;
                !was
            };
            if ended {
                turn.poke();
            }
        }
    }

    /// After `close`: let the open streams write their [DONE] and the 0 chunk, for at most `timeout` seconds
    /// ([`CLOSE_SETTLE_SECS`](crate::link_chat::CLOSE_SETTLE_SECS)), before the server cancels their connections. Without it the cancel landed first and
    /// the app saw a cut stream on every daemon restart (review, 2026-10-08).
    pub async fn settle(&self, timeout: f64) {
        let now = Instant::now();
        let deadline = now.checked_add(secs(timeout)).unwrap_or(now + Duration::from_secs(365 * 24 * 3600));
        loop {
            let mut rx = self.inner.streams_changed.subscribe();
            if self.inner.state.lock().streams == 0 {
                return;
            }
            if tokio::time::timeout_at(deadline, rx.changed()).await.is_err() {
                return;
            }
        }
    }

    // -- where eve's lines go --

    /// One of eve's lines from work app turn `turn_id` began (TelegramInlet.app_out) → whether a stream is reading
    /// it. Its own turn's stream while that is open; once it ended on a question, the newest open stream (the owner's
    /// answer, sent from the app, whose stream carries the rest of the reply). False: no stream will show it, so the
    /// inlet sends it to Telegram. An "ask" ends the turn's stream after it.
    pub fn deliver(&self, turn_id: &str, text: &str, kind: &str) -> bool {
        let turn = {
            let state = self.inner.state.lock();
            let mut turn = state.turns.get(turn_id).cloned();
            if let Some(t) = turn.clone() {
                let (done, asked) = {
                    let s = t.state.lock();
                    (s.done, s.asked)
                };
                if done {
                    turn = if asked { Self::newest_open(&state) } else { None };
                }
            }
            turn
        };
        let Some(turn) = turn else { return false };
        let readers = {
            let mut s = turn.state.lock();
            s.chunks.push(text.to_string());
            if kind == "ask" {
                s.asked = true;
            }
            s.readers
        };
        turn.poke();
        readers > 0
    }

    fn newest_open(state: &DoorState) -> Option<Arc<Turn>> {
        state.turns.values().rev().find(|t| !t.done()).cloned()
    }

    fn prune(&self, state: &mut DoorState) {
        let now = self.inner.clock();
        let ids: Vec<String> = state.turns.keys().cloned().collect();
        for tid in ids {
            let Some(turn) = state.turns.get(&tid).cloned() else { continue };
            if turn.done() && (now - turn.born >= DEDUPE_SECS || state.turns.len() > MAX_KEPT_TURNS) {
                state.turns.shift_remove(&tid);
                if !turn.client_id.is_empty()
                    && state.by_client.get(&turn.client_id).is_some_and(|t| Arc::ptr_eq(t, &turn))
                {
                    state.by_client.remove(&turn.client_id);
                }
            }
        }
    }

    // -- HTTP --

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
        if headers.get("origin").is_some_and(|o| !o.is_empty()) {
            return Self::write_json(writer, 403, &json!({"ok": false, "error": "the Eve Link app only"})).await;
        }
        let auth = headers.get("authorization").map(String::as_str).unwrap_or("");
        let (scheme, presented) = auth.split_once(' ').unwrap_or((auth, ""));
        let allowed = scheme.to_lowercase() == "bearer" && token_check.is_some_and(|check| check(tpy::strip(presented)));
        if !allowed {
            return Self::write_json(writer, 401, &json!({"ok": false, "error": "the link token is missing or wrong"}))
                .await;
        }
        let action = JOB_ACTION_RE.captures(path);
        let app_open = APP_OPEN_RE.captures(path);
        let control = CONTROL_RE.captures(path);
        let file = FILE_RE.captures(path);
        let session = SESSION_RE.captures(path);
        let group = |c: &Option<regex::Captures<'_>>, n: usize| -> Option<String> {
            c.as_ref().and_then(|c| c.get(n)).map(|m| m.as_str().to_string())
        };
        let verb = group(&action, 2);
        let control_name = group(&control, 1);
        let session_verb = group(&session, 2);
        let known = PATHS.iter().any(|p| p == path)
            || app_open.is_some()
            || file.is_some()
            || session.is_some()
            || verb.as_deref().is_some_and(|v| JOB_ACTIONS.contains(&v))
            || control_name.as_deref().is_some_and(|n| CONTROLS.iter().any(|c| c == n));
        if !known {
            return Self::write_json(writer, 404, &json!({"ok": false, "error": "not found"})).await;
        }
        if (path == JOBS_PATH || path == MUSE_PATH) && (method == "GET" || method == "POST") {
            // GET lists or reads; POST starts a job or flips Muse
        } else {
            let wants_post = POST_PATHS.contains(&path)
                || action.is_some()
                || app_open.is_some()
                || control.is_some()
                || session_verb.is_some();
            if method != if wants_post { "POST" } else { "GET" } {
                return Self::write_json(writer, 405, &json!({"ok": false, "error": "wrong method"})).await;
            }
        }
        if let Some(id) = group(&file, 1) {
            return self.file(&id, writer).await;
        }
        if path == SESSIONS_PATH || path == SESSION_START_PATH || session.is_some() {
            let sid = group(&session, 1);
            return self.sessions(path, sid, session_verb, headers, reader, writer).await;
        }
        if path == JOBS_PATH || action.is_some() {
            let action = action.as_ref().map(|_| (group(&action, 1).unwrap_or_default(), verb.unwrap_or_default()));
            return self.jobs(headers, reader, writer, action, method).await;
        }
        if path == APPS_PATH || app_open.is_some() {
            return self.apps(apps, headers, writer, group(&app_open, 1)).await;
        }
        if path == UPLOAD_PATH {
            return self.upload(headers, reader, writer).await;
        }
        if path == CONTROLS_PATH || control.is_some() {
            return self.controls(apps, headers, reader, writer, control_name).await;
        }
        if let Some(name) = STREAM_PATHS.get(path) {
            let target = apps.as_ref().and_then(|a| a.hub(name));
            return match target.filter(|t| t.available()) {
                None => {
                    let line = NO_CONTROL.get(name).cloned().unwrap_or_default();
                    Self::write_json(writer, 404, &json!({"ok": false, "line": line})).await
                }
                Some(target) => target.stream(writer).await,
            };
        }
        if path == TODAY_PATH {
            let got = self.inner.today.gather((self.inner.inlet)(), apps).await;
            return Self::write_json(writer, 200, &Value::Object(got)).await;
        }
        if path == UPDATES_PATH || path == UPDATES_DISMISS_PATH || path == UPDATES_REFRESH_PATH {
            return self.updates_route(path, headers, reader, writer).await;
        }
        if path == LOCATION_PATH {
            return self.location(headers, reader, writer).await;
        }
        if path == VISIT_PATH || path == PLACES_PATH || path == PLACES_FORGET_PATH {
            return self.places(path, headers, reader, writer).await;
        }
        let inlet = match (self.inner.inlet)() {
            Some(inlet) if inlet.chat_ready() => inlet,
            _ => return Self::write_json(writer, 503, &json!({"ok": false, "error": NO_CHAT})).await,
        };
        self.attach(&inlet);
        if path == MUSE_PATH {
            return self.muse(&inlet, method, headers, reader, writer).await;
        }
        if path == TOPICS_PATH || path == TOPICS_BOOKMARK_PATH {
            return self.topics(&inlet, path, query, headers, reader, writer).await;
        }
        if path == TURN_PATH {
            self.turn(&inlet, headers, reader, writer).await
        } else {
            self.messages(&inlet, query, writer).await
        }
    }

    async fn turn(
        &self,
        inlet: &Arc<dyn Inlet>,
        headers: &Headers,
        reader: &mut DynRead,
        writer: &mut DynWrite,
    ) -> std::io::Result<()> {
        let size = length(headers);
        if size <= 0 || size > MAX_BODY_BYTES {
            Self::discard(reader, size).await;
            let status = if size > MAX_BODY_BYTES { 413 } else { 400 };
            return Self::write_json(writer, status, &json!({"ok": false, "error": "the body must be JSON of at most 64 kB"}))
                .await;
        }
        let body = read_exactly(reader, size as usize, BODY_SECS).await.and_then(|d| loads(&d));
        let body = match body {
            Some(Value::Object(m)) => Some(m),
            _ => None,
        };
        let text = body.as_ref().and_then(|b| get(b, "text")).cloned();
        let client_id = match body.as_ref() {
            Some(b) => match get(b, "client_id") {
                None | Some(Value::Null) => Some(Value::String(String::new())),
                Some(v) => Some(v.clone()),
            },
            None => Some(Value::String(String::new())),
        };
        let (Some(Value::String(text)), Some(Value::String(client_id))) = (text, client_id) else {
            return Self::write_json(writer, 400, &json!({"ok": false, "error": "text and client_id must be strings"}))
                .await;
        };
        let text = tpy::strip(&text).to_string();
        let ids = body.as_ref().and_then(|b| get(b, "attachments")).cloned().unwrap_or(json!([]));
        let ids: Option<Vec<String>> = match &ids {
            Value::Array(list) if list.len() <= MAX_ATTACHMENTS => {
                list.iter().map(|v| v.as_str().map(str::to_string)).collect::<Option<Vec<_>>>()
            }
            _ => None,
        };
        let ids = ids.filter(|ids| {
            let mut seen = std::collections::HashSet::new();
            ids.iter().all(|i| seen.insert(i.clone()))
        });
        let Some(ids) = ids else {
            let error = format!("attachments must be at most {MAX_ATTACHMENTS} different upload ids");
            return Self::write_json(writer, 400, &json!({"ok": false, "error": error})).await;
        };
        let mut uploads = Vec::new();
        for id in ids {
            let store = Arc::clone(&self.inner.uploads);
            match blocking(move || store.get(&id)).await {
                Some(u) => uploads.push(u),
                None => {
                    let error = "an attachment is unknown or expired: attach it again";
                    return Self::write_json(writer, 400, &json!({"ok": false, "error": error})).await;
                }
            }
        }
        let inline: u64 = uploads.iter().filter(|u| INLINE_KINDS.contains(&u.kind.as_str())).map(|u| u.size).sum();
        if inline > MAX_INLINE_BYTES {
            let error = format!(
                "those photos and PDFs are more than {} MB together: send fewer at once",
                MAX_INLINE_BYTES / (1024 * 1024)
            );
            return Self::write_json(writer, 413, &json!({"ok": false, "error": error})).await;
        }
        let least = if uploads.is_empty() { 1 } else { 0 };
        if !(least..=MAX_TEXT_CHARS).contains(&chars(&text)) {
            let error = format!("text must be 1 to {MAX_TEXT_CHARS} characters");
            return Self::write_json(writer, 400, &json!({"ok": false, "error": error})).await;
        }
        if chars(&client_id) > MAX_CLIENT_ID_CHARS {
            let error = format!("client_id is at most {MAX_CLIENT_ID_CHARS}");
            return Self::write_json(writer, 400, &json!({"ok": false, "error": error})).await;
        }
        let grade = match body.as_ref().map(|b| get(b, "grade")) {
            Some(None) | None => Some("auto".to_string()),
            Some(Some(Value::String(g))) if GRADES.contains(&g.as_str()) => Some(g.clone()),
            _ => None,
        };
        let Some(grade) = grade else {
            return Self::write_json(writer, 400, &json!({"ok": false, "error": "grade must be auto, instant or think"}))
                .await;
        };
        let (turn, fresh) = {
            let mut state = self.inner.state.lock();
            self.prune(&mut state);
            let kept = if client_id.is_empty() { None } else { state.by_client.get(&client_id).cloned() };
            match kept {
                Some(turn) => {
                    turn.add_reader(1);
                    (turn, false)
                }
                None => {
                    let id = turn_id(&client_id, &|t: &str| state.turns.contains_key(t));
                    let turn = Turn::new(id, client_id.clone(), self.inner.clock());
                    // This request is the turn's reader before the words are heard: a line said before the stream's
                    // first write must go to the app, never to Telegram for want of a reader.
                    turn.add_reader(1);
                    Self::register(&mut state, &turn);
                    (turn, true)
                }
            }
        };
        if fresh {
            self.start(inlet, &turn, &text, &grade, uploads);
        } else {
            tracing::info!("link chat: the same client_id again; its turn {} is streamed, not heard twice", turn.id);
        }
        self.stream(&turn, reader, writer).await
    }

    /// `_start`'s first half: the turn is kept for dedupe and for `deliver`.
    fn register(state: &mut DoorState, turn: &Arc<Turn>) {
        state.turns.insert(turn.id.clone(), Arc::clone(turn));
        if !turn.client_id.is_empty() {
            state.by_client.insert(turn.client_id.clone(), Arc::clone(turn));
        }
    }

    /// `_start`'s second half (the turn is registered): hear the words, typed, as the app's, and drive the turn.
    fn start(&self, inlet: &Arc<dyn Inlet>, turn: &Arc<Turn>, text: &str, grade: &str, uploads: Vec<eve_link_uploads::Upload>) {
        let count = uploads.len();
        let opts = HearOptions { on_call: false, app: Some(turn.id.clone()), grade: grade.to_string(), attachments: uploads };
        // typed: answered in the app
        // the failure is logged without its message: a message can quote what was said
        if inlet.hear(text, opts).is_err() {
            tracing::warn!("link chat: the turn could not start (error)");
            turn.state.lock().done = true;
            return;
        }
        tracing::info!("link chat: turn {} started ({} chars, {} attachments, {})", turn.id, chars(text), count, grade);
        let task = tokio::spawn(Self::drive(Arc::downgrade(&self.inner), Arc::clone(turn), Arc::clone(inlet)));
        let mut state = self.inner.state.lock();
        state.drivers.retain(|t| !t.is_finished());
        state.drivers.push(task);
    }

    /// End the turn's stream when no turn the owner started is in flight, when it asked the owner something, or at
    /// `turn_secs`. Polled every `settle_secs`, and woken at once by a new line.
    async fn drive(shared: Weak<Shared>, turn: Arc<Turn>, inlet: Arc<dyn Inlet>) {
        let _end = DriveEnd { shared: shared.clone(), turn: Arc::clone(&turn) };
        let Some((clock, turn_secs, settle_secs)) =
            shared.upgrade().map(|s| (Arc::clone(&s.clock), s.opts.turn_secs, s.opts.settle_secs))
        else {
            return;
        };
        loop {
            let mut rx = turn.changed.subscribe();
            let (done, asked) = {
                let s = turn.state.lock();
                (s.done, s.asked)
            };
            if done || asked || clock() - turn.born >= turn_secs {
                break;
            }
            if !inlet.owner_busy() {
                break;
            }
            let _ = tokio::time::timeout(secs(settle_secs), rx.changed()).await;
        }
    }

    /// Write the turn's lines as SSE chunks as they come, then [DONE]. The caller has counted this request as one of
    /// the turn's readers. A client that hangs up stops counting at once (its EOF), so the rest goes to Telegram; the
    /// stream itself ends at the first failed write, as a half-closed connection may still be reading.
    async fn stream(&self, turn: &Arc<Turn>, reader: &mut DynRead, writer: &mut DynWrite) -> std::io::Result<()> {
        self.inner.state.lock().streams += 1;
        let mut end = StreamEnd { shared: Arc::clone(&self.inner), turn: Arc::clone(turn), reading: true };
        let ping_secs = self.inner.opts.ping_secs;
        let mut one = [0u8; 1];
        let eof = reader.read(&mut one);
        tokio::pin!(eof);
        let mut eof_done = false;
        let mut sent = 0usize;
        let result: std::io::Result<()> = async {
            writer.write_all(STREAM_HEAD).await?;
            writer.flush().await?;
            loop {
                let mut changed = turn.changed.subscribe();
                // one look under the lock: the next line, or the end, or nothing yet
                enum Next {
                    Line(String),
                    End,
                    Wait,
                }
                loop {
                    let next = {
                        let s = turn.state.lock();
                        if sent < s.chunks.len() {
                            Next::Line(s.chunks[sent].clone())
                        } else if s.done {
                            Next::End
                        } else {
                            Next::Wait
                        }
                    };
                    match next {
                        Next::Line(text) => {
                            let content = if sent == 0 { text } else { format!("\n\n{text}") };
                            chunk(writer, &sse(&content)).await?;
                            sent += 1;
                        }
                        Next::End => {
                            chunk(writer, DONE).await?;
                            writer.write_all(b"0\r\n\r\n").await?;
                            writer.flush().await?;
                            return Ok(());
                        }
                        Next::Wait => break,
                    }
                }
                let woke = tokio::select! {
                    _ = changed.changed() => true,
                    got = &mut eof, if !eof_done => {
                        eof_done = true;
                        let hung_up = !matches!(got, Ok(n) if n > 0);
                        if end.reading && hung_up {
                            end.reading = false; // hung up: what comes next goes to Telegram
                            turn.add_reader(-1);
                        }
                        true
                    }
                    _ = tokio::time::sleep(secs(ping_secs)) => false,
                };
                if !woke {
                    chunk(writer, PING).await?;
                }
            }
        }
        .await;
        drop(end);
        // a failed write ends the stream; the connection is the caller's to close
        let _ = result;
        Ok(())
    }

    async fn topics(
        &self,
        inlet: &Arc<dyn Inlet>,
        path: &str,
        query: &str,
        headers: &Headers,
        reader: &mut DynRead,
        writer: &mut DynWrite,
    ) -> std::io::Result<()> {
        use crate::link_topics;

        let store = match inlet.transcripts() {
            Some(store) if store.enabled => store,
            _ => return Self::write_json(writer, 503, &json!({"ok": false, "error": NO_MEMORY})).await,
        };
        if path == TOPICS_PATH {
            let q = parse_qsl(query);
            let raw = q.get("limit").filter(|s| !s.is_empty()).cloned();
            let limit = match raw {
                None => Some(link_topics::DEFAULT_LIMIT as i64),
                Some(raw) => py_int(&raw),
            };
            let Some(limit) = limit else {
                return Self::write_json(writer, 400, &json!({"ok": false, "error": "limit must be a whole number"}))
                    .await;
            };
            let limit = limit.clamp(1, link_topics::MAX_LIMIT as i64) as usize;
            let bookmarks = Arc::clone(&self.inner.bookmarks);
            let titler = Arc::clone(&self.inner.titler);
            let view = blocking(move || link_topics::view(&store, &bookmarks, limit, Some(&titler))).await;
            let count = |k: &str| view.get(k).and_then(Value::as_array).map(Vec::len).unwrap_or(0);
            tracing::info!("link chat: topics for the app: {} ({} saved)", count("topics"), count("bookmarked"));
            return Self::write_json(writer, 200, &Value::Object(view)).await;
        }
        let Some(body) = Self::optional_body(headers, reader, writer).await? else { return Ok(()) };
        let tid = get(&body, "id").and_then(Value::as_str).filter(|t| link_topics::ID_RE.is_match(t)).map(str::to_string);
        let on = get(&body, "on").and_then(Value::as_bool);
        let (Some(tid), Some(on)) = (tid, on) else {
            let error = "the body must be {\"id\": a topic's id, \"on\": true or false}";
            return Self::write_json(writer, 400, &json!({"ok": false, "error": error})).await;
        };
        if on {
            let (s, t) = (Arc::clone(&store), tid.clone());
            if !blocking(move || link_topics::known(&s, &t)).await {
                return Self::write_json(writer, 404, &json!({"ok": false, "error": "no such topic"})).await;
            }
        }
        let bookmarks = Arc::clone(&self.inner.bookmarks);
        blocking(move || bookmarks.set(&tid, on, None)).await?;
        Self::write_json(writer, 200, &json!({"ok": true, "bookmarked": on})).await
    }

    async fn messages(&self, inlet: &Arc<dyn Inlet>, query: &str, writer: &mut DynWrite) -> std::io::Result<()> {
        let q = parse_qsl(query);
        if let Some(before) = q.get("before").filter(|b| !b.is_empty()) {
            return self.older(inlet, before, writer).await;
        }
        let after = q.get("after").filter(|a| !a.is_empty()).cloned();
        let wait = match q.get("wait").filter(|w| !w.is_empty()) {
            None => Some(0.0),
            Some(raw) => py_float(raw),
        };
        let Some(wait) = wait else {
            return Self::write_json(writer, 400, &json!({"ok": false, "error": "wait must be a number of seconds"}))
                .await;
        };
        // not clamp: a NaN wait (Python's float("nan")) is no wait at all, not a NaN deadline
        #[allow(clippy::manual_clamp)]
        let wait = wait.max(0.0).min(WAIT_MAX_SECS);
        let store = match inlet.transcripts() {
            Some(store) if store.enabled => store,
            _ => return Self::write_json(writer, 503, &json!({"ok": false, "error": NO_MEMORY})).await,
        };
        let Ok((after, seen_version)) = split_cursor(after.as_deref()) else {
            return Self::write_json(writer, 400, &json!({"ok": false, "error": "bad cursor"})).await;
        };
        let reactions = inlet.app_reactions();
        // the app holds an older picture of the reactions than this one: answer now, with the new one
        let stale = || match (&reactions, seen_version) {
            (Some(r), Some(seen)) => r.version() != seen,
            _ => false,
        };
        let deadline = Instant::now() + secs(if after.is_some() { wait } else { 0.0 });
        let was_busy = Self::busy(inlet.as_ref());
        let (mut rows, cursor) = loop {
            let mut woken = self.inner.appended.subscribe(); // taken before the read: no append is missed
            let (s, a) = (Arc::clone(&store), after.clone());
            let Ok((rows, cursor)) = blocking(move || read_messages(&s, a.as_deref())).await else {
                return Self::write_json(writer, 400, &json!({"ok": false, "error": "bad cursor"})).await;
            };
            let left = deadline.saturating_duration_since(Instant::now());
            // A change of `busy` also ends the wait: eve's last line is written while the turn is still in flight, so
            // without this the app's "replying" dots stayed up until the poll's 25 s ran out.
            if !rows.is_empty() || left.is_zero() || Self::busy(inlet.as_ref()) != was_busy || stale() {
                break (rows, cursor);
            }
            let _ = tokio::time::timeout(left.min(secs(self.inner.opts.poll_secs)), woken.changed()).await;
        };
        rows = self.thumbs(rows).await;
        let mut payload = Map::new();
        let mut cursor = cursor;
        let mut kept_reactions = None;
        if let Some(reactions) = &reactions {
            let (version, kept) = (reactions.version(), reactions.snapshot());
            for row in rows.iter_mut() {
                if let Some(rid) = row.get("rid").and_then(Value::as_str) {
                    if let Some(r) = kept.get(rid) {
                        let r = r.clone();
                        row.insert("reaction".into(), r);
                    }
                }
            }
            cursor = format!("{cursor}~{version}");
            kept_reactions = Some(kept);
        }
        payload.insert("messages".into(), Value::Array(rows.into_iter().map(Value::Object).collect()));
        payload.insert("cursor".into(), json!(cursor));
        payload.insert("busy".into(), json!(Self::busy(inlet.as_ref())));
        if let Some(kept) = kept_reactions {
            payload.insert("reactions".into(), Value::Object(kept));
        }
        Self::write_json(writer, 200, &Value::Object(payload)).await
    }

    /// `GET /api/link/messages?before=<message id>`: the page of history just before that message, for infinite
    /// scroll: `{"messages", "more", "busy"}`. Never waits and never moves the live poll's cursor.
    async fn older(&self, inlet: &Arc<dyn Inlet>, before: &str, writer: &mut DynWrite) -> std::io::Result<()> {
        let store = match inlet.transcripts() {
            Some(store) if store.enabled => store,
            _ => return Self::write_json(writer, 503, &json!({"ok": false, "error": NO_MEMORY})).await,
        };
        let (s, b) = (Arc::clone(&store), before.to_string());
        let Ok((rows, more)) = blocking(move || read_before(&s, Some(&b), HISTORY_LIMIT)).await else {
            return Self::write_json(writer, 400, &json!({"ok": false, "error": "before must be a message id"})).await;
        };
        let mut rows = self.thumbs(rows).await;
        if let Some(reactions) = inlet.app_reactions() {
            let kept = reactions.snapshot();
            for row in rows.iter_mut() {
                if let Some(rid) = row.get("rid").and_then(Value::as_str) {
                    if let Some(r) = kept.get(rid) {
                        let r = r.clone();
                        row.insert("reaction".into(), r);
                    }
                }
            }
        }
        let rows: Vec<Value> = rows.into_iter().map(Value::Object).collect();
        let busy = Self::busy(inlet.as_ref());
        Self::write_json(writer, 200, &json!({"messages": rows, "more": more, "cursor": "", "busy": busy})).await
    }

    /// Each attachment on the page of rows gets its `thumb` (images) and `gone` (expired) now. Blocking, so run on a
    /// worker thread.
    async fn thumbs(&self, rows: Vec<Obj>) -> Vec<Obj> {
        let uploads = Arc::clone(&self.inner.uploads);
        blocking(move || {
            let mut rows = rows;
            for row in rows.iter_mut() {
                let Some(Value::Array(atts)) = row.get_mut("attachments") else { continue };
                for a in atts.iter_mut() {
                    let Value::Object(a) = a else { continue };
                    let id = a.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                    let alive = uploads.get(&id).is_some();
                    if !alive {
                        a.insert("gone".into(), json!(true));
                    } else if a.get("kind").and_then(Value::as_str) == Some("image") {
                        if let Some(thumb) = uploads.thumb_url(&id).filter(|t| !t.is_empty()) {
                            a.insert("thumb".into(), json!(thumb));
                        }
                    }
                }
            }
            rows
        })
        .await
    }

    /// `POST /api/link/upload`: the raw bytes, kept by link_uploads.UploadStore → its id. Needs no chat running: the
    /// turn that uses it does.
    async fn upload(&self, headers: &Headers, reader: &mut DynRead, writer: &mut DynWrite) -> std::io::Result<()> {
        let chunked = headers.get("transfer-encoding").is_some_and(|t| t.to_lowercase().contains("chunked"));
        if chunked {
            return Self::write_json(writer, 411, &json!({"ok": false, "error": "send the upload with a Content-Length"}))
                .await;
        }
        let size = length(headers);
        let max_bytes = self.inner.uploads.max_bytes as i64;
        if size > max_bytes {
            Self::discard(reader, size).await;
            return Self::write_json(writer, 413, &json!({"ok": false, "error": "that file is too large: 20 MB at most"}))
                .await;
        }
        if size <= 0 {
            return Self::write_json(writer, 400, &json!({"ok": false, "error": "the upload is empty"})).await;
        }
        let Some(data) = read_exactly(reader, size as usize, UPLOAD_SECS).await else {
            return Self::write_json(writer, 400, &json!({"ok": false, "error": "the upload did not arrive whole"})).await;
        };
        let raw_name = headers.get("x-filename").map(String::as_str).unwrap_or("");
        let name: String = unquote(raw_name, false).chars().take(1000).collect();
        let store = Arc::clone(&self.inner.uploads);
        match blocking(move || store.save(&data, &name)).await {
            Ok(upload) => {
                let mut out = Map::new();
                out.insert("ok".into(), json!(true));
                if let Value::Object(public) = upload.public() {
                    out.extend(public);
                }
                Self::write_json(writer, 200, &Value::Object(out)).await
            }
            Err(eve_link_uploads::StoreError::Upload(e)) => {
                Self::write_json(writer, e.status, &json!({"ok": false, "error": e.message})).await
            }
            Err(eve_link_uploads::StoreError::Io(e)) => {
                tracing::warn!("link chat: an upload could not be kept ({})", tpy::os_error_name(&e));
                Self::write_json(writer, 500, &json!({"ok": false, "error": "eve couldn't keep that file"})).await
            }
        }
    }

    /// `GET /api/link/file/<id>`: a kept upload's bytes, for the app to open full size. `UploadStore.get` takes
    /// nothing but 32 hex characters and checks the expiry, so no other file is reachable. Needs no chat running.
    async fn file(&self, upload_id: &str, writer: &mut DynWrite) -> std::io::Result<()> {
        let store = Arc::clone(&self.inner.uploads);
        let id = upload_id.to_string();
        let found = blocking(move || {
            let upload = store.get(&id)?;
            let data = std::fs::read(&upload.path).ok()?;
            Some((upload, data))
        })
        .await;
        let Some((upload, data)) = found else {
            let error = "that file is gone (kept for a day) or never was";
            return Self::write_json(writer, 404, &json!({"ok": false, "error": error})).await;
        };
        // an image or PDF opens in place; anything else is a download, and an active type (HTML, SVG, script) is
        // never served as itself (link_uploads.served)
        let (mime, disposition) = eve_link_uploads::served(&upload.kind, &upload.mime);
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {mime}\r\nContent-Disposition: {disposition}\r\nContent-Length: {}\r\n\
             Cache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
            data.len()
        );
        let mut out = head.into_bytes();
        out.extend_from_slice(&data);
        writer.write_all(&out).await?;
        writer.flush().await?;
        tracing::info!("link chat: file {} sent ({}, {} bytes)", upload.id, upload.kind, data.len());
        Ok(())
    }

    /// `GET /api/link/jobs` or `POST /api/link/jobs/<id>/<action>`: the chief's cards and its own methods, as the
    /// Telegram code words call them (telegram.TelegramInlet._chief_command). `POST /api/link/jobs`: a New job
    /// (`_new_job`).
    async fn jobs(
        &self,
        headers: &Headers,
        reader: &mut DynRead,
        writer: &mut DynWrite,
        action: Option<(String, String)>,
        method: &str,
    ) -> std::io::Result<()> {
        let inlet = (self.inner.inlet)();
        let chief = inlet.as_ref().and_then(|i| i.chief());
        let (Some(inlet), Some(chief)) = (inlet, chief) else {
            if method == "POST" {
                Self::discard(reader, length(headers)).await;
            }
            return Self::write_json(writer, 503, &json!({"ok": false, "error": NO_CHIEF})).await;
        };
        let Some((cid, verb)) = action else {
            if method == "POST" {
                return self.new_job(&inlet, &chief, headers, reader, writer).await;
            }
            let listed = list_jobs(chief.as_ref(), &self.inner.hidden.ids());
            return Self::write_json(writer, 200, &Value::Object(listed)).await;
        };
        let Some(body) = Self::optional_body(headers, reader, writer).await? else { return Ok(()) };
        let rev = match get(&body, "rev") {
            None | Some(Value::Null) => None,
            Some(Value::Number(n)) if n.is_i64() => n.as_i64(),
            Some(Value::Number(n)) if n.is_u64() => Some(i64::MAX),
            Some(_) => {
                return Self::write_json(writer, 400, &json!({"ok": false, "error": "rev must be a whole number"})).await;
            }
        };
        let Some(card) = chief.card(&cid) else {
            return Self::write_json(writer, 404, &json!({"ok": false, "reason": format!("there is no card {cid}")}))
                .await;
        };
        if verb == "go" && card.doubtful {
            // via="id" skips the chief's own in-doubt refusal (typed "go c12" is a deliberate re-run); a tap is not
            let reason = format!("{cid} may already have done that; say whether it happened first");
            return Self::write_json(writer, 200, &json!({"ok": false, "reason": reason})).await;
        }
        if verb == "hide" {
            if !card.terminal {
                let reason = format!("{cid} is still open; drop it first");
                return Self::write_json(writer, 200, &json!({"ok": false, "reason": reason})).await;
            }
            if let Err(e) = self.inner.hidden.add(&cid) {
                tracing::warn!("link chat: could not hide {} ({})", cid, tpy::os_error_name(&e));
                let reason = "I couldn't remove that just now.";
                return Self::write_json(writer, 500, &json!({"ok": false, "reason": reason})).await;
            }
            tracing::info!("link chat: hide {} from the app: ok", cid);
            return Self::write_json(writer, 200, &json!({"ok": true, "hidden": cid})).await;
        }
        let call = async {
            match verb.as_str() {
                "go" => chief.approve(&cid, "id", rev).await, // the owner's yes, as "go c12" in Telegram
                "done" => chief.done(&cid).await,
                "drop" => chief.drop_job(&cid).await,
                "happened" => chief.happened(&cid).await,
                "didnt" => chief.didnt(&cid).await,
                _ => chief.raise_budget(&cid).await,
            }
        };
        let result = match AssertUnwindSafe(call).catch_unwind().await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) | Err(_) => {
                tracing::warn!("link chat: the chief's {} on {} failed (error)", verb, cid);
                let reason = "I couldn't do that just now.";
                return Self::write_json(writer, 500, &json!({"ok": false, "reason": reason})).await;
            }
        };
        let ok = result.get("ok").is_some_and(tpy::truthy);
        tracing::info!("link chat: {} {} from the app: {}", verb, cid, if ok { "ok" } else { "refused" });
        Self::write_json(writer, 200, &Value::Object(result)).await
    }

    /// `POST /api/link/jobs`: the owner's goal heard as an app turn led by `JOB_LEAD`; the card is whatever that turn's
    /// take_on filed. Nothing here parses, files or approves a card: the brain and the chief do, on their own path,
    /// with every gate they keep (a one-way step still waits on its Go).
    async fn new_job(
        &self,
        inlet: &Arc<dyn Inlet>,
        chief: &Arc<dyn Chief>,
        headers: &Headers,
        reader: &mut DynRead,
        writer: &mut DynWrite,
    ) -> std::io::Result<()> {
        let Some(body) = Self::optional_body(headers, reader, writer).await? else { return Ok(()) };
        let goal = get(&body, "goal").cloned().unwrap_or(Value::Null);
        let detail = match get(&body, "detail") {
            None | Some(Value::Null) => Value::String(String::new()),
            Some(v) => v.clone(),
        };
        let client_id = get(&body, "client_id").cloned().unwrap_or_else(|| Value::String(String::new()));
        let (Value::String(goal), Value::String(detail), Value::String(client_id)) = (goal, detail, client_id) else {
            let error = "goal, detail and client_id must be strings";
            return Self::write_json(writer, 400, &json!({"ok": false, "error": error})).await;
        };
        let (goal, detail) = (tpy::strip(&goal).to_string(), tpy::strip(&detail).to_string());
        if !(1..=MAX_GOAL_CHARS).contains(&chars(&goal)) {
            let error = format!("goal must be 1 to {MAX_GOAL_CHARS} characters");
            return Self::write_json(writer, 400, &json!({"ok": false, "error": error})).await;
        }
        if chars(&detail) > MAX_DETAIL_CHARS {
            let error = format!("detail is at most {MAX_DETAIL_CHARS} characters");
            return Self::write_json(writer, 400, &json!({"ok": false, "error": error})).await;
        }
        if chars(&client_id) > MAX_CLIENT_ID_CHARS {
            let error = format!("client_id is at most {MAX_CLIENT_ID_CHARS}");
            return Self::write_json(writer, 400, &json!({"ok": false, "error": error})).await;
        }
        if !inlet.chat_ready() {
            return Self::write_json(writer, 503, &json!({"ok": false, "error": NO_CHAT})).await;
        }
        self.attach(inlet);
        let (turn, fresh) = {
            let mut state = self.inner.state.lock();
            self.prune(&mut state);
            let now = self.inner.clock();
            state.job_starts.retain(|_, old| !(old.done() && now - old.born >= DEDUPE_SECS));
            let kept = if client_id.is_empty() { None } else { state.job_starts.get(&client_id).cloned() };
            match kept {
                Some(turn) => {
                    turn.add_reader(1);
                    (turn, false)
                }
                None => {
                    let id = turn_id("", &|t: &str| state.turns.contains_key(t));
                    let turn = Turn::new(id, String::new(), now);
                    // This request reads the turn before the words are heard: its lines come back here, never to
                    // Telegram for want of a reader (as _turn does).
                    turn.add_reader(1);
                    Self::register(&mut state, &turn);
                    if !client_id.is_empty() {
                        // Kept under the lock that looked it up: a retry racing this one on another thread waits on
                        // this turn (Python's single loop made the look-up and the keep one step).
                        state.job_starts.insert(client_id.clone(), Arc::clone(&turn));
                    }
                    (turn, true)
                }
            }
        };
        if fresh {
            let words = if detail.is_empty() {
                format!("{JOB_LEAD}{goal}")
            } else {
                format!("{JOB_LEAD}{goal}\n\n{JOB_DETAIL_LEAD}{detail}")
            };
            self.start(inlet, &turn, &words, "auto", Vec::new());
            let failed = {
                let s = turn.state.lock();
                s.done && s.chunks.is_empty()
            };
            if failed {
                turn.add_reader(-1);
                if !client_id.is_empty() {
                    let mut state = self.inner.state.lock();
                    if state.job_starts.get(&client_id).is_some_and(|t| Arc::ptr_eq(t, &turn)) {
                        state.job_starts.remove(&client_id); // never kept: a retry is heard anew
                    }
                }
                let reason = "eve couldn't start that just now.";
                return Self::write_json(writer, 500, &json!({"ok": false, "reason": reason})).await;
            }
        } else {
            tracing::info!("link chat: the same New job again; its turn {} is waited on, not heard twice", turn.id);
        }
        let card = {
            let _reading = ReaderGuard(Arc::clone(&turn));
            self.await_card(inlet, chief, &turn).await
        };
        let (said, done): (Vec<String>, bool) = {
            let s = turn.state.lock();
            let said = s.chunks.iter().map(|c| tpy::strip(&strip_openui(c)).to_string()).filter(|c| !c.is_empty()).collect();
            (said, s.done)
        };
        tracing::info!(
            "link chat: New job, turn {} ({}+{} chars): {}",
            turn.id,
            chars(&goal),
            chars(&detail),
            card.as_ref().map(|c| c.id.as_str()).unwrap_or("no card")
        );
        if let Some(card) = card {
            let job = Value::Object(job_entry(&card));
            Self::write_json(writer, 200, &json!({"ok": true, "job": job, "said": said})).await
        } else if done {
            let reason = "eve answered without starting a job";
            Self::write_json(writer, 200, &json!({"ok": false, "said": said, "reason": reason})).await
        } else {
            let reason = "eve is still on it; the job shows in Work once it is filed";
            Self::write_json(writer, 200, &json!({"ok": false, "said": said, "reason": reason})).await
        }
    }

    /// The card this turn's own take_on filed (`TelegramInlet.app_filed`, recorded against the turn's id), or None,
    /// once the turn is over; the turn gets at most `job_secs`, and `JOB_GRACE_SECS` once the card is there (its
    /// backbrief is said right after it is filed). A card another turn files meanwhile (a chat turn, a second New
    /// job) is never this one's.
    async fn await_card(
        &self,
        inlet: &Arc<dyn Inlet>,
        chief: &Arc<dyn Chief>,
        turn: &Arc<Turn>,
    ) -> Option<crate::seams::JobCard> {
        let mut found_at: Option<f64> = None;
        loop {
            let mut changed = turn.changed.subscribe(); // taken before the read: no poke is missed
            let cid = inlet.app_filed(&turn.id).filter(|c| !c.is_empty());
            let card = cid.and_then(|c| chief.card(&c));
            let now = self.inner.clock();
            if card.is_some() && found_at.is_none() {
                found_at = Some(now);
            }
            if turn.done()
                || now - turn.born >= self.inner.opts.job_secs
                || found_at.is_some_and(|at| now - at >= JOB_GRACE_SECS)
            {
                return card;
            }
            let _ = tokio::time::timeout(secs(self.inner.opts.settle_secs), changed.changed()).await;
        }
    }

    /// A request's JSON object body, {} when it has none; None when it was refused (the answer is written).
    async fn optional_body(headers: &Headers, reader: &mut DynRead, writer: &mut DynWrite) -> std::io::Result<Option<Obj>> {
        let size = length(headers);
        if size == 0 {
            return Ok(Some(Map::new()));
        }
        if !(0..=MAX_BODY_BYTES).contains(&size) {
            Self::discard(reader, size).await;
            let status = if size > MAX_BODY_BYTES { 413 } else { 400 };
            Self::write_json(writer, status, &json!({"ok": false, "error": "the body must be JSON of at most 64 kB"}))
                .await?;
            return Ok(None);
        }
        match read_exactly(reader, size as usize, BODY_SECS).await.and_then(|d| loads(&d)) {
            Some(Value::Object(body)) => Ok(Some(body)),
            _ => {
                Self::write_json(writer, 400, &json!({"ok": false, "error": "the body must be a JSON object"})).await?;
                Ok(None)
            }
        }
    }

    /// `GET /api/link/updates`: the cards (updates.Updates.view). `POST .../dismiss` `{"id"}`: that card's sources are
    /// dismissed for 30 days (404 when the store does not know the id). `POST .../refresh`: one rebuild starts in the
    /// background; the answer never waits for it.
    async fn updates_route(
        &self,
        path: &str,
        headers: &Headers,
        reader: &mut DynRead,
        writer: &mut DynWrite,
    ) -> std::io::Result<()> {
        let updates = Arc::clone(&self.inner.updates);
        if path == UPDATES_PATH {
            let view = blocking(move || updates.view()).await;
            return Self::write_json(writer, 200, &Value::Object(view)).await;
        }
        let Some(body) = Self::optional_body(headers, reader, writer).await? else { return Ok(()) };
        if path == UPDATES_REFRESH_PATH {
            let started = updates.refresh();
            return Self::write_json(writer, 200, &json!({"ok": true, "started": started})).await;
        }
        let cid = match get(&body, "id") {
            Some(Value::String(cid)) if !tpy::strip(cid).is_empty() && chars(cid) <= MAX_CARD_ID_CHARS => {
                tpy::strip(cid).to_string()
            }
            _ => return Self::write_json(writer, 400, &json!({"ok": false, "error": "id must be a card's id"})).await,
        };
        if !blocking(move || updates.dismiss(&cid)).await {
            // an id the store does not know (not now, not among the cards a recent rebuild replaced): nothing was
            // dismissed, so say so; the app puts the card back and reads the list again
            return Self::write_json(writer, 404, &json!({"ok": false, "error": "no such card"})).await;
        }
        Self::write_json(writer, 200, &json!({"ok": true})).await
    }

    /// `POST /api/link/location`: the phone's latest fix (phone_location.py). 403 while EVE_PHONE_LOCATION is off, 400
    /// for a body that is not a fix; an older fix than the one kept is answered ok and ignored. The log says the fix's
    /// age and accuracy, never where it is.
    async fn location(&self, headers: &Headers, reader: &mut DynRead, writer: &mut DynWrite) -> std::io::Result<()> {
        let environ = self.inner.opts.environ.clone();
        if !phone_location::enabled(environ.as_ref()) {
            Self::discard(reader, length(headers)).await;
            return Self::write_json(writer, 403, &json!({"ok": false, "error": NO_LOCATION})).await;
        }
        let Some(body) = Self::optional_body(headers, reader, writer).await? else { return Ok(()) };
        let fix = match phone_location::parse(&Value::Object(body), None) {
            Ok(fix) => fix,
            Err(e) => return Self::write_json(writer, 400, &json!({"ok": false, "error": e.0})).await,
        };
        let (f, env) = (fix.clone(), environ.clone());
        let kept = blocking(move || phone_location::store(&f, Some(&phone_location::state_path(env.as_ref())))).await;
        let kept = match kept {
            Ok(kept) => kept,
            Err(e) => {
                tracing::warn!("link chat: the location was not kept ({})", tpy::os_error_name(&e));
                let error = "the Mac could not keep the location";
                return Self::write_json(writer, 500, &json!({"ok": false, "error": error})).await;
            }
        };
        tracing::info!(
            "link chat: location {} ({})",
            if kept { "updated" } else { "ignored, not newer" },
            phone_location::describe(&fix, None)
        );
        if places::enabled(environ.as_ref()) {
            // into the history as well (a late fix too: the history keeps it; the stay ignores what is behind it)
            let env = environ.clone();
            let closed = blocking(move || places::Places::new(None, env, None).record_fix(&fix)).await;
            match closed {
                Ok(Some(closed)) if !closed.is_empty() => {
                    tracing::info!("link chat: places: a stay ended (read from fixes)");
                }
                Ok(_) => {}
                Err(e) => {
                    tracing::warn!("link chat: places: the fix was not kept in the history ({})", e.kind_name());
                }
            }
        }
        Self::write_json(writer, 200, &json!({"ok": true})).await
    }

    /// `POST /api/link/visit` (an iOS visit), `GET /api/link/places` (the counts) and `POST /api/link/places/forget`
    /// (everything, with the latest fix). 403 while places are off. Logs say what was done, never where.
    async fn places(&self, path: &str, headers: &Headers, reader: &mut DynRead, writer: &mut DynWrite) -> std::io::Result<()> {
        let environ = self.inner.opts.environ.clone();
        if !places::enabled(environ.as_ref()) {
            if path != PLACES_PATH {
                Self::discard(reader, length(headers)).await;
            }
            return Self::write_json(writer, 403, &json!({"ok": false, "error": NO_PLACES})).await;
        }
        let store = Arc::new(places::Places::new(None, environ, None));
        if path == PLACES_PATH {
            let s = Arc::clone(&store);
            return match blocking(move || s.summary()).await {
                Ok(summary) => {
                    let mut out = Map::new();
                    out.insert("ok".into(), json!(true));
                    out.insert("on".into(), json!(true));
                    out.extend(summary);
                    Self::write_json(writer, 200, &Value::Object(out)).await
                }
                Err(e) => {
                    tracing::warn!("link chat: places: the summary failed ({})", e.kind_name());
                    let error = "the Mac could not read the places";
                    Self::write_json(writer, 500, &json!({"ok": false, "error": error})).await
                }
            };
        }
        let Some(body) = Self::optional_body(headers, reader, writer).await? else { return Ok(()) };
        if path == PLACES_FORGET_PATH {
            let s = Arc::clone(&store);
            return match blocking(move || s.forget()).await {
                Ok(forgot) => {
                    let forgot: Map<String, Value> = forgot.into_iter().map(|(k, v)| (k, json!(v))).collect();
                    Self::write_json(writer, 200, &json!({"ok": true, "forgot": forgot})).await
                }
                Err(e) => {
                    tracing::warn!("link chat: places: forget failed ({})", e.kind_name());
                    let error = "the Mac could not forget the places";
                    Self::write_json(writer, 500, &json!({"ok": false, "error": error})).await
                }
            };
        }
        let visit = match places::parse_visit(&Value::Object(body), None) {
            Ok(visit) => visit,
            Err(e) => return Self::write_json(writer, 400, &json!({"ok": false, "error": e.0})).await,
        };
        let accuracy = visit.accuracy_m;
        let s = Arc::clone(&store);
        match blocking(move || s.record_visit(&visit)).await {
            Ok(done) => {
                tracing::info!("link chat: places: visit {} (accuracy {} m)", done, accuracy.round_ties_even() as i64);
                Self::write_json(writer, 200, &json!({"ok": true})).await
            }
            Err(e) => {
                tracing::warn!("link chat: places: the visit was not kept ({})", e.kind_name());
                Self::write_json(writer, 500, &json!({"ok": false, "error": "the Mac could not keep the visit"})).await
            }
        }
    }

    /// The Work tab's Claude Code sessions (work_sessions.py, docs/work-sessions.md): `GET /api/link/sessions`,
    /// `POST .../start` `{"task", "client_id"?}`, `GET .../<id>`, `POST .../<id>/reply` `{"text"}` and
    /// `POST .../<id>/stop`. The token and Origin checks in `serve` come first; 503 while they are off.
    async fn sessions(
        &self,
        path: &str,
        sid: Option<String>,
        verb: Option<String>,
        headers: &Headers,
        reader: &mut DynRead,
        writer: &mut DynWrite,
    ) -> std::io::Result<()> {
        let work = self.inner.opts.work.as_ref().and_then(|f| f());
        let Some(work) = work else {
            if path == SESSION_START_PATH || verb.is_some() {
                Self::discard(reader, length(headers)).await; // the body is read and dropped before the 503
            }
            return Self::write_json(writer, 503, &json!({"ok": false, "error": NO_WORK})).await;
        };
        let (status, out) = if path == SESSIONS_PATH {
            work.list().await
        } else if path == SESSION_START_PATH {
            let Some(body) = Self::optional_body(headers, reader, writer).await? else { return Ok(()) };
            let task = get(&body, "task").cloned().unwrap_or(Value::Null);
            let client_id = get(&body, "client_id").cloned().unwrap_or_else(|| Value::String(String::new()));
            work.start(&task, &client_id).await
        } else {
            let sid = sid.unwrap_or_default();
            match verb.as_deref() {
                None => work.detail(&sid).await,
                Some("reply") => {
                    let Some(body) = Self::optional_body(headers, reader, writer).await? else { return Ok(()) };
                    let text = get(&body, "text").cloned().unwrap_or(Value::Null);
                    work.reply(&sid, &text).await
                }
                Some(_) => work.stop(&sid).await?,
            }
        };
        Self::write_json(writer, status, &out).await
    }

    /// `GET /api/link/apps`: the owner's built apps, last changed first. `POST /api/link/apps/<slug>/open`: that app's
    /// page at the tunnel's origin with a fresh app token; 404 for an app that does not exist.
    async fn apps(
        &self,
        apps: Option<Arc<dyn MiniApp>>,
        headers: &Headers,
        writer: &mut DynWrite,
        slug: Option<String>,
    ) -> std::io::Result<()> {
        let (Some(server), Some(store)) = (apps.clone(), apps.as_ref().and_then(|a| a.app_store())) else {
            return Self::write_json(writer, 503, &json!({"ok": false, "error": NO_APPS})).await;
        };
        let Some(slug) = slug else {
            let infos = blocking(move || store.list()).await;
            let listed: Vec<Value> =
                infos.into_iter().map(|a| json!({"slug": a.slug, "name": a.title, "updated": a.updated})).collect();
            return Self::write_json(writer, 200, &json!({"apps": listed})).await;
        };
        let s = slug.clone();
        if blocking(move || store.info(&s)).await.is_none() {
            return Self::write_json(writer, 404, &json!({"ok": false, "error": "there is no such app"})).await;
        }
        tracing::info!("link chat: app {} opened from the app", slug);
        Self::write_json(writer, 200, &json!({"url": server.app_link(&slug, headers)})).await
    }

    /// `GET /api/link/controls`: which of eve's apps and panels this Mac has (controls_listing).
    /// `POST /api/link/controls/<name>`: that app's or panel's `panel(body)`, its JSON as it is. The objects are the
    /// Mini App server's own (set by the daemon), read now, so a card that came or went since is seen.
    async fn controls(
        &self,
        server: Option<Arc<dyn MiniApp>>,
        headers: &Headers,
        reader: &mut DynRead,
        writer: &mut DynWrite,
        name: Option<String>,
    ) -> std::io::Result<()> {
        let get_hub = |n: &str| server.as_ref().and_then(|s| s.hub(n));
        let Some(name) = name else {
            return Self::write_json(writer, 200, &Value::Object(controls_listing(&get_hub))).await;
        };
        let Some(target) = get_hub(&name).filter(|t| t.available()) else {
            let line = NO_CONTROL.get(&name).cloned().unwrap_or_default();
            return Self::write_json(writer, 404, &json!({"ok": false, "line": line})).await;
        };
        let Some(body) = Self::optional_body(headers, reader, writer).await? else { return Ok(()) };
        let body = Value::Object(body);
        let result = match AssertUnwindSafe(target.panel(&body)).catch_unwind().await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) | Err(_) => {
                tracing::warn!("link chat: the {} control failed (error)", name);
                return Self::write_json(writer, 500, &json!({"ok": false, "line": "That didn't work just now."})).await;
            }
        };
        let action = body.get("action");
        let held = matches!(action.and_then(Value::as_str), Some("drive") | Some("hold"));
        if !held {
            // a held arrow (Pika, the camera) beats every 150 ms
            let said = match action.filter(|a| tpy::truthy(a)) {
                Some(Value::String(s)) => s.clone(),
                // str(value): Python's repr for a number, a list or a dict
                Some(other) => eve_app_registry::py_repr(other),
                None => "status".to_string(),
            };
            let said: String = said.chars().take(24).collect();
            let ok = result.as_object().and_then(|r| r.get("ok")).map(tpy::truthy).unwrap_or(true);
            tracing::info!("link chat: {} {} from the app: {}", name, said, if ok { "ok" } else { "refused" });
        }
        Self::write_json(writer, 200, &result).await
    }

    /// `/api/link/muse`: the composer's eve/Muse switch (telegram.set_muse, muse_pi.py). GET reads it; POST
    /// `{"on": bool}` flips it, quietly (the switch itself shows the change, so no line is posted to the chat).
    async fn muse(
        &self,
        inlet: &Arc<dyn Inlet>,
        method: &str,
        headers: &Headers,
        reader: &mut DynRead,
        writer: &mut DynWrite,
    ) -> std::io::Result<()> {
        if method == "POST" {
            let Some(body) = Self::optional_body(headers, reader, writer).await? else { return Ok(()) };
            let Some(on) = get(&body, "on").and_then(Value::as_bool) else {
                return Self::write_json(writer, 400, &json!({"ok": false, "error": "expected {\"on\": true|false}"}))
                    .await;
            };
            inlet.set_muse(on, false);
        }
        let out = json!({"ok": true, "available": eve_muse_pi::available(), "on": inlet.muse()});
        Self::write_json(writer, 200, &out).await
    }

    /// The payload's `busy`: a turn the owner started is in flight and is not just waiting on the owner's answer to
    /// a question (no "replying" dots under eve's own question).
    pub fn busy(inlet: &dyn crate::seams::Inlet) -> bool {
        inlet.owner_busy() && !inlet.waiting_on_owner()
    }

    /// `_json`: `HTTP/1.1 <status> <reason>`, `Content-Type: application/json`, `Content-Length`,
    /// `Cache-Control: no-store`, `Connection: close`, and the body as Python's `json.dumps` writes it.
    pub async fn write_json(writer: &mut DynWrite, status: u16, obj: &Value) -> std::io::Result<()> {
        let body = py_json(obj).into_bytes();
        let head = format!(
            "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\n\
             Connection: close\r\n\r\n",
            reason(status),
            body.len()
        );
        let mut out = head.into_bytes();
        out.extend_from_slice(&body);
        writer.write_all(&out).await?;
        writer.flush().await
    }

    /// Read and drop a refused body (at most DISCARD_MAX_BYTES, for DISCARD_SECS) before the refusal is written.
    /// Answering and closing while the client still sends made the client see "connection reset" instead of the 413
    /// (a test caught it about once in 35 runs, 2026-10-08). A larger body is still cut off.
    pub async fn discard(reader: &mut DynRead, size: i64) {
        let mut left = size.clamp(0, DISCARD_MAX_BYTES);
        let drain = async {
            let mut buf = vec![0u8; 65536];
            while left > 0 {
                let want = left.min(65536) as usize;
                match reader.read(&mut buf[..want]).await {
                    Ok(0) | Err(_) => return,
                    Ok(n) => left -= n as i64,
                }
            }
        };
        let _ = tokio::time::timeout(secs(DISCARD_SECS), drain).await;
    }
}
