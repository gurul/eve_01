//! The text door and what it is lent: `_make_apps`, `_make_direct`, `_make_telegram`, `_make_chief`,
//! `_make_canvas`, `_make_share`, `_make_agentcard`, `_make_america`, `_make_meeter`, `_ask_owner_on_phone`,
//! `_tell_owner_on_phone`, `_start_miniapp`, `_start_phone_expressions`.
//!
//! Python lent the door bound methods of the daemon (`self._photo_for_owner`, `self._set_sound`, ...); here each is a
//! closure holding the daemon's `Arc`, as a bound method holds `self`. The objects the door takes as a trait the
//! daemon's own type does not implement (the scene watcher, the room notes, the Memory and Notes cards, the direct
//! Calendar for the Meet notetaker) are wrapped by the small adapters at the end of this file.

use std::collections::BTreeSet;
use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use futures::future::BoxFuture;
use futures::FutureExt;
use serde_json::{json, Map, Value};

use crate::daemon::Daemon;
use crate::seams::{Inlet, Job, Obj};

/// Python's `repr()` of a str: single quotes unless the text holds a single quote and no double quote.
fn py_repr_str(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') { '"' } else { '\'' };
    let mut out = String::with_capacity(s.len() + 2);
    out.push(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `repr()` of a `dict[str, str]` (a toolkit policy, as the start lines log it).
pub fn py_repr_policy(policy: &indexmap::IndexMap<String, String>) -> String {
    let items: Vec<String> = policy.iter().map(|(k, v)| format!("{}: {}", py_repr_str(k), py_repr_str(v))).collect();
    format!("{{{}}}", items.join(", "))
}

/// A panic's message, as `str(exc)` would read.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "panic".to_string()
    }
}

/// `try: ... except Exception:` around a factory: a panic in it is the exception Python's guard caught.
fn guarded<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    std::panic::catch_unwind(AssertUnwindSafe(f)).map_err(|p| panic_text(p.as_ref()))
}

/// `threading.Thread(target=start, daemon=True).start()`: the start runs beside the daemon, never holding it. On the
/// daemon's own runtime when there is one (so the clients it makes keep their runtime), else on a thread of its own.
fn start_beside(name: &'static str, fut: impl Future<Output = ()> + Send + 'static) {
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => {
            handle.spawn(fut);
        }
        Err(_) => {
            let spawned = std::thread::Builder::new().name(name.to_string()).spawn(move || {
                match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                    Ok(rt) => rt.block_on(fut),
                    Err(e) => tracing::warn!("{name}: no runtime to start on ({e})"),
                }
            });
            if let Err(e) = spawned {
                tracing::warn!("{name}: could not start a thread ({e})");
            }
        }
    }
}

/// Run `fut` to its end from synchronous code: in place on a multi-threaded runtime, else on a thread of its own.
fn run_to_end<T: Send>(fut: impl Future<Output = T> + Send) -> T {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        if handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread {
            return tokio::task::block_in_place(|| handle.block_on(fut));
        }
    }
    std::thread::scope(|scope| {
        scope
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("a current-thread runtime builds")
                    .block_on(fut)
            })
            .join()
            .unwrap_or_else(|p| std::panic::resume_unwind(p))
    })
}

/// An executor's error as the chief logs it: the chief's own [`eve_chief::Raised`] when it is one, else the error's
/// text under Python's catch-all name.
fn raised(e: anyhow::Error) -> eve_chief::Raised {
    match e.downcast_ref::<eve_chief::Raised>() {
        Some(r) => r.clone(),
        None => eve_chief::Raised::new("Exception", e.to_string()),
    }
}

/// `type(exc).__name__` for what `america.make_america()` raises: ValueError for an unknown EVE_AMERICA_TRANSPORT.
fn america_error_name(e: &anyhow::Error) -> &'static str {
    if e.downcast_ref::<eve_america::InvalidTransport>().is_some() {
        "ValueError"
    } else {
        "RuntimeError"
    }
}

impl Daemon {
    /// `_make_apps(owner_ids)`: Composio's apps (Slack, Notion, Linear, work mail and calendar) for the chat
    /// brain, None when off or keyless.
    ///
    /// EVE_COMPOSIO=1 with a key: the owner's Composio session (composio_tools.py), started beside the daemon so a
    /// slow or down Composio never holds it; the text brain offers its tools once it is up. Gmail read only, the
    /// calendar writable, the rest asked first (the module).
    pub fn make_apps(&self, owner_ids: &BTreeSet<i64>) -> Option<Arc<eve_composio_tools::ComposioBridge>> {
        let cfg = eve_composio_tools::configured(self.environ(), owner_ids);
        if !cfg.enabled {
            tracing::info!("apps: off (EVE_COMPOSIO)");
            return None;
        }
        let user_id = cfg.user_id.clone();
        let bridge = Arc::new(eve_composio_tools::ComposioBridge::new(cfg, None));
        let environ = self.environ.clone();
        let starting = bridge.clone();
        start_beside("composio-start", async move {
            match starting.start().await {
                Ok(()) => tracing::info!(
                    "apps: Composio session up for {}; policy {}",
                    user_id,
                    py_repr_policy(&eve_composio_tools::toolkit_policy(environ.as_ref()))
                ),
                // the text brain simply has no app tools this run
                Err(e) => tracing::warn!("apps: Composio did not start ({}: {})", e.type_name(), e),
            }
        });
        Some(bridge)
    }

    /// `_make_direct()`: the direct Google/GitHub/Maps bridge, None when off.
    ///
    /// EVE_APPS=1: the owner's direct apps (apps_mcp.py): Gmail, Calendar and Drive through Google's APIs, GitHub and
    /// Maps through their MCP servers, each up only when signed in (`apps login google`, the env file's tokens).
    /// Composio (`make_apps`) keeps Slack, Notion and Linear. `AppsBridge::start` needs the bridge to itself, so here
    /// it runs before the bridge is shared (see PARITY.md, Differences).
    pub fn make_direct(&self) -> Option<Arc<eve_apps_mcp::AppsBridge>> {
        let cfg = eve_apps_mcp::configured(self.environ());
        if !cfg.enabled {
            tracing::info!("apps: direct off (EVE_APPS)");
            return None;
        }
        let mut bridge = eve_apps_mcp::AppsBridge::new(cfg);
        if bridge.available().is_empty() {
            tracing::info!("apps: direct on, but nothing is signed in (eve-bridge apps login google)");
        }
        let started = guarded(|| run_to_end(bridge.start()));
        match started {
            Ok(()) => {
                let apps = bridge.apps();
                let up = if apps.is_empty() { "none".to_string() } else { apps.join(", ") };
                tracing::info!("apps: direct up: {}; policy {}", up, py_repr_policy(&bridge.policy));
            }
            // the text brain simply has no direct app tools this run
            Err(_) => tracing::warn!("apps: direct did not start (PanicException)"),
        }
        Some(Arc::new(bridge))
    }

    /// `_make_telegram()`: the text door with everything it is lent (the chief, the watcher, the meeter, the apps,
    /// the vault, the chat log, Canvas, purchases, sharing, the scene, the head, the notes, the terminal, the
    /// Claude sessions, the app hubs); stored as `telegram` and `telegram_inlet`. None unless EVE_TELEGRAM is on
    /// with a token and an owner id.
    ///
    /// The inlet is lent the same agent factory, camera, slow brain and memory (the same Memory, None while memory
    /// is off) the voice session gets, the owner's apps (direct: Google, GitHub, Maps; Composio: Slack, Notion,
    /// Linear) and their second brain (the vault). None — today's behaviour — otherwise.
    pub fn make_telegram(&self) -> Option<Arc<eve_brain::telegram::TelegramInlet>> {
        use eve_brain::seams::{BrainMemory, RoomNotes, SceneWatch, Sessions};

        let d = self.me();
        let env = self.environ();
        let memory = self.st.lock().memory.clone();

        let tg = eve_brain::telegram::configured(env);
        if tg.enabled {
            tracing::info!("telegram: web search {}", tg.search.engine);
        }
        let vault = eve_second_brain::configured(env);
        if tg.enabled {
            let line =
                if vault.enabled { format!("on at {}", vault.root.display()) } else { "off (EVE_SECOND_BRAIN)".into() };
            tracing::info!("second brain: {line}");
        }
        // The watcher texts through this door, so it exists only with it. Made here, started beside the door.
        let watcher = if tg.enabled {
            match guarded(|| eve_web::watch::make_watcher(None)) {
                Ok(w) => w.map(Arc::new),
                // a watcher that cannot start costs the watching, never the daemon
                Err(e) => {
                    tracing::error!("watch: could not start; watching is off this run ({e})");
                    None
                }
            }
        } else {
            None
        };
        self.st.lock().watcher = watcher.clone();
        let apps = if tg.enabled { self.make_apps(&tg.owners()) } else { None };
        let direct = if tg.enabled { self.make_direct() } else { None };
        let meeter = if tg.enabled { self.make_meeter(direct.as_ref()) } else { None };
        self.st.lock().meeter = meeter.clone();
        let agentcard = if tg.enabled && eve_openrouter::available(env) { self.make_agentcard() } else { None };
        self.st.lock().agentcard = agentcard.clone();
        // The chat log (chat_log.py): on for every install unless EVE_CHAT_LOG=0, memory on or off, so a quiet
        // spell or a restart never empties the chat's history. Forget reaches it through the Memory, when one is lent.
        let chat_log = if tg.enabled {
            match eve_chat_log::open_configured(env) {
                Ok(log) => log.map(Arc::new),
                Err(e) => {
                    tracing::error!("chat log: could not open ({e}); the chat's history is RAM only this run");
                    None
                }
            }
        } else {
            None
        };
        if let (Some(log), Some(memory)) = (&chat_log, &memory) {
            memory.set_chat_log(Some(log.clone() as Arc<dyn eve_memory::ChatLogLayer>));
        }

        let app_hubs = self.app_hubs(true);
        let canvas = self.make_canvas();
        let share = if tg.enabled { self.make_share() } else { None };
        let america = if tg.enabled && eve_openrouter::available(env) { self.make_america() } else { None };
        let opentable_enabled = eve_opentable::configured(env);

        let brief: eve_brain::seams::StrFn = {
            let cfg = self.recall_cfg.clone();
            let memory = memory.clone();
            Arc::new(move || {
                let transcripts = memory.as_ref().map(|m| m.transcripts.clone());
                eve_records::opening_brief(
                    &cfg,
                    transcripts.as_deref().map(|t| t as &dyn eve_records::BriefSource),
                    None,
                )
            })
        };
        let on_photo: eve_brain::seams::OnPhoto = {
            let d = d.clone();
            Arc::new(move |note: String| {
                let d = d.clone();
                async move { d.photo_for_owner(&note).await }.boxed()
            })
        };
        let on_state: eve_brain::seams::OnState = {
            let d = d.clone();
            Arc::new(move |state: &str| d.on_agent_state(state))
        };
        let busy: eve_brain::seams::BoolFn = {
            let d = d.clone();
            Arc::new(move || d.desk_has_the_mac())
        };
        let on_explore: eve_brain::seams::OnExplore = {
            let d = d.clone();
            Arc::new(move || {
                let d = d.clone();
                async move { d.request_explore("requested from Telegram").await.map_err(|e| anyhow::anyhow!(e.0)) }
                    .boxed()
            })
        };
        let on_sound: eve_brain::seams::OnSound = {
            let d = d.clone();
            Arc::new(move |on: bool| d.set_sound(on))
        };
        let on_caption: eve_brain::seams::OnCaption = {
            let d = d.clone();
            Arc::new(move |msg: Value| {
                let msg = match msg {
                    Value::Object(m) => m,
                    _ => Obj::new(),
                };
                d.on_caption(msg)
            })
        };
        let notes: eve_brain::seams::NotesFn = {
            let d = d.clone();
            Arc::new(move || Arc::new(LentNotes(d.room_notes_taker())) as Arc<dyn RoomNotes>)
        };
        let terminal: eve_brain::seams::Terminal = {
            let d = d.clone();
            Arc::new(move |cwd: String, text: String| {
                let d = d.clone();
                async move { Ok(d.type_into_terminal(&cwd, &text).await) }.boxed()
            })
        };
        // Live sessions only: one whose terminal died without a SessionEnd is dropped (claude_live).
        // Awaited by the inlet: the ps/lsof probe runs on a worker thread, never on this loop.
        let claude_sessions: eve_brain::seams::ClaudeSessions = {
            let d = d.clone();
            Arc::new(move || {
                let d = d.clone();
                Ok(Sessions::Later(
                    async move {
                        let mut book = StateBook(d);
                        Ok(eve_claude_live::picker_sessions_off_loop(&mut book, None).await)
                    }
                    .boxed(),
                ))
            })
        };
        let (thinker, scene) = (self.st.lock().thinker.clone(), self.scene.clone());

        let lent = eve_brain::telegram::InletOptions {
            agent_factory: Some(self.agent_factory()),
            agent_enabled: self.agent_cfg.enabled,
            busy: Some(busy),
            brief: Some(brief),
            on_photo: Some(on_photo),
            thinker,
            on_state: Some(on_state),
            memory: memory.clone().map(|m| m as Arc<dyn BrainMemory>),
            apps,
            direct,
            vault: (tg.enabled && vault.enabled).then(|| Arc::new(vault)),
            chat_log,
            watcher: watcher.clone(),
            meeter,
            canvas,
            agentcard: agentcard
                .clone()
                .map(|card| eve_brain::seams::CardPurchases::new(card) as Arc<dyn eve_brain::seams::Purchases>),
            america,
            opentable_enabled,
            share,
            scene: Some(Arc::new(LentScene(scene)) as Arc<dyn SceneWatch>),
            head: Some(self.head.clone()),
            on_explore: Some(on_explore),
            on_sound: Some(on_sound),
            on_caption: Some(on_caption),
            notes: Some(notes),
            terminal: Some(terminal),
            claude_sessions: Some(claude_sessions),
            app_hubs,
            environ: self.environ.clone(),
            ..Default::default()
        };
        let inlet = eve_brain::telegram::make_inlet(tg, lent);
        match &inlet {
            Some(inlet) => {
                let chief = self.make_chief(inlet, watcher);
                *inlet.chief.lock() = chief;
            }
            None => {
                if let Some(card) = agentcard {
                    card.close();
                    self.st.lock().agentcard = None;
                }
            }
        }
        {
            let mut st = self.st.lock();
            st.telegram = inlet.clone().map(|i| i as Arc<dyn Inlet>);
            st.telegram_inlet = inlet.clone();
        }
        inlet
    }

    /// `_make_chief(inlet, watcher)`: the chief of staff, None when off (eve_chief::enabled).
    ///
    /// The chief of staff (chief.py) when EVE_CHIEF has it on, else None (today's behaviour: the door is byte for
    /// byte what it was). It is built from the door itself: a Mac step runs through the door's task slot
    /// (chief_start, chief_stop, chief_mac_free), a Go is the door's question (chief_ask), a message goes through
    /// tell_owner (chief_notify), and the options call through the door's own Responses creator. A chief that
    /// cannot be built costs the chief, never the door.
    pub fn make_chief(
        &self,
        inlet: &Arc<eve_brain::telegram::TelegramInlet>,
        watcher: Option<Arc<eve_web::watch::Watcher>>,
    ) -> Option<Arc<eve_chief::Chief>> {
        use eve_chief::Raised;

        let env = self.environ();
        if !eve_chief::enabled(env) {
            tracing::info!("chief: off (EVE_CHIEF={})", eve_chief::mode(env));
            return None;
        }
        // The chief lives on the inlet (`inlet.chief`): it reaches the inlet weakly, so neither keeps the other.
        let me = Arc::downgrade(inlet);
        let gone = || Raised::new("RuntimeError", "the chat has stopped");
        let start_task: eve_chief::StartTask = {
            let me = me.clone();
            Arc::new(move |args: eve_chief::StartArgs| {
                let me = me.clone();
                async move {
                    let inlet = me.upgrade().ok_or_else(gone)?;
                    let n = i64::try_from(args.n).unwrap_or(i64::MAX);
                    Ok(Value::Object(inlet.chief_start(&args.goal, &args.card_id, n, args.floor.as_deref())))
                }
                .boxed()
            })
        };
        let stop_task: eve_chief::StopTask = {
            let me = me.clone();
            Arc::new(move |card_id: String, n: usize| {
                let me = me.clone();
                async move {
                    let inlet = me.upgrade().ok_or_else(gone)?;
                    inlet.chief_stop(&card_id, i64::try_from(n).unwrap_or(i64::MAX));
                    Ok(())
                }
                .boxed()
            })
        };
        let mac_free: eve_chief::MacFree = {
            let me = me.clone();
            Arc::new(move || {
                let inlet = me.upgrade().ok_or_else(gone)?;
                Ok(inlet.chief_mac_free())
            })
        };
        let create: eve_chief::Create = {
            let create = inlet.door_create();
            Arc::new(move |body: Value| {
                let create = create.clone();
                async move { create(body).await.map_err(raised) }.boxed()
            })
        };
        let ask: eve_chief::Ask = {
            let me = me.clone();
            Arc::new(move |text: String| {
                let me = me.clone();
                async move {
                    let inlet = me.upgrade().ok_or_else(gone)?;
                    inlet.chief_ask(&text).await.map_err(raised)
                }
                .boxed()
            })
        };
        let notify: eve_chief::Notify = {
            let me = me.clone();
            Arc::new(move |text: String, about: String| {
                let me = me.clone();
                async move {
                    let inlet = me.upgrade().ok_or_else(gone)?;
                    Ok(inlet.chief_notify(&text, &about).await)
                }
                .boxed()
            })
        };
        // watch=getattr(watcher, "add", None); watch_ids={w.id for w in watcher.watches}
        let (watch, watch_ids): (Option<eve_chief::WatchAdd>, Option<eve_chief::WatchIds>) = match &watcher {
            Some(w) => {
                let adder = w.clone();
                let add: eve_chief::WatchAdd = Arc::new(move |args: Value| {
                    let adder = adder.clone();
                    async move {
                        let args = match args {
                            Value::Object(m) => m,
                            _ => Map::new(),
                        };
                        Ok(adder.add(&args).await)
                    }
                    .boxed()
                });
                let lister = w.clone();
                let ids: eve_chief::WatchIds =
                    Arc::new(move || Ok(lister.watches().iter().map(|w| w.lock().id.clone()).collect()));
                (Some(add), Some(ids))
            }
            None => (None, None),
        };
        let opts = eve_chief::MakeOptions {
            start_task: Some(start_task),
            stop_task: Some(stop_task),
            mac_free: Some(mac_free),
            watch,
            watch_ids,
            create: Some(create),
            ask: Some(ask),
            notify: Some(notify),
            environ: self.environ.clone(),
        };
        let chief = match guarded(|| eve_chief::make_chief(opts)) {
            Ok(chief) => chief,
            Err(e) => {
                tracing::error!("chief: could not start; it is off this run ({e})");
                return None;
            }
        };
        if let Some(chief) = &chief {
            tracing::info!(
                "chief: on (EVE_CHIEF={}); {} open card(s)",
                eve_chief::mode(env),
                chief.ledger.open().len()
            );
        }
        chief
    }

    /// `_make_canvas()`: Canvas, read only, made once (`canvas`).
    ///
    /// The owner's Canvas (canvas.py), read only, for the Telegram brain; None when CANVAS_BASE_URL or
    /// CANVAS_API_TOKEN is unset. Made once per daemon.
    pub fn make_canvas(&self) -> Option<Arc<eve_canvas::Canvas>> {
        if let Some(made) = self.st.lock().canvas.clone() {
            return made;
        }
        let canvas = match guarded(|| eve_canvas::make_canvas(self.environ(), None)) {
            Ok(c) => c.map(Arc::new),
            // a Canvas that cannot load costs Canvas, never the daemon
            Err(e) => {
                tracing::error!("canvas: could not load; Canvas is off this run ({e})");
                None
            }
        };
        self.st.lock().canvas = Some(canvas.clone());
        canvas
    }

    /// `_make_share()`: sharing, made once (`share`).
    ///
    /// Sharing the owner's files with someone (share.py): iMessage on this Mac, and email as a Drive link when the
    /// direct apps' Google sign-in can write to Drive (share.google_token, a file check, no network). None unless
    /// EVE_SHARE=1. Made once per daemon (2026-10-04).
    pub fn make_share(&self) -> Option<Arc<eve_brain::share::Share>> {
        if let Some(made) = self.st.lock().share.clone() {
            return made;
        }
        let env = self.environ();
        let share = match guarded(|| eve_brain::share::make_share(env, eve_brain::share::google_token(env))) {
            Ok(s) => s.map(Arc::new),
            // sharing that cannot load costs sharing, never the daemon
            Err(e) => {
                tracing::error!("share: could not load; sharing is off this run ({e})");
                None
            }
        };
        self.st.lock().share = Some(share.clone());
        share
    }

    /// `_make_agentcard()`: purchases, made once (`agentcard`).
    ///
    /// Optional user-scoped purchasing for the shared chat/call brain. No live request at startup.
    pub fn make_agentcard(&self) -> Option<Arc<eve_agentcard::AgentCard>> {
        match guarded(|| eve_agentcard::make_agentcard(self.environ())) {
            Ok(card) => card.map(Arc::new),
            // credentials and provider bodies must never enter logs
            Err(_) => {
                tracing::warn!("agentcard: could not load (PanicException); purchasing is off this run");
                None
            }
        }
    }

    /// `_make_america()`: U.S. law and government sources, made once (`america`); a failure logs and is None.
    ///
    /// Optional America.gov legal information for the shared chat/call brain; no startup request.
    pub fn make_america(&self) -> Option<Arc<eve_america::America>> {
        if let Some(made) = self.st.lock().america.clone() {
            return made;
        }
        let america = match guarded(|| eve_america::make_america(self.environ())) {
            Ok(Ok(a)) => a.map(Arc::new),
            // keep provider data and credentials out of logs
            Ok(Err(e)) => {
                tracing::warn!(
                    "america: could not load ({}); legal information is off this run",
                    america_error_name(&e)
                );
                None
            }
            Err(_) => {
                tracing::warn!("america: could not load (PanicException); legal information is off this run");
                None
            }
        };
        self.st.lock().america = Some(america.clone());
        america
    }

    /// `_make_meeter(direct)`: the Meet notetaker, None when off.
    ///
    /// The Meet notetaker (meet.py): joins from the owner's Chrome (attach mode) on a browser lane of its own, reads
    /// the owner's calendar through the direct Calendar (apps_mcp, Google's API) for "join my 3pm", and writes the
    /// notes beside the room notes. Chrome's "Allow remote debugging?" for its connection goes through the same broker
    /// as the Chrome lane's. None when it is off or cannot run.
    pub fn make_meeter(&self, apps: Option<&Arc<eve_apps_mcp::AppsBridge>>) -> Option<Arc<eve_meet::Meeter>> {
        let d = self.me();
        let ask: eve_chrome_consent::AskOwner = {
            let d = d.clone();
            Arc::new(move |q: String| {
                let d = d.clone();
                async move { d.ask_owner_checked(&q).await }.boxed()
            })
        };
        let tell: eve_chrome_consent::TellOwner = {
            let d = d.clone();
            Arc::new(move |text: String| {
                let d = d.clone();
                async move {
                    d.tell_owner_on_phone(&text).await;
                    Ok(())
                }
                .boxed()
            })
        };
        let preference = match self.environ() {
            Some(env) => eve_chrome_consent::access_preference_in(env),
            None => eve_chrome_consent::access_preference(),
        };
        let broker = eve_chrome_consent::ConsentBroker::new(
            Some(ask),
            eve_chrome_consent::BrokerOptions {
                tell_owner: Some(tell),
                auto_allow: preference == "allow",
                ..Default::default()
            },
        );
        let answer: eve_meet::Answer = Arc::new(move || {
            let broker = broker.clone();
            async move {
                broker.answer_own_connection().await;
            }
            .boxed()
        });
        let calendar = apps.map(|a| Arc::new(DirectCalendar(a.clone())) as Arc<dyn eve_meet::CalendarApps>);
        let notes_root = eve_notes::notes_dir(&self.recall_cfg);
        match guarded(|| eve_meet::make_meeter(None, &notes_root, Some(answer), calendar, self.environ())) {
            Ok(meeter) => meeter.map(Arc::new),
            // a notetaker that cannot start costs the meetings, never the daemon
            Err(e) => {
                tracing::error!("meet: could not start; joining meetings is off this run ({e})");
                None
            }
        }
    }

    /// `_ask_owner_on_phone(question)` as the Chrome consent broker calls it: the owner's answer, or the error
    /// Python raised (no chat to ask in, or another question already waiting).
    pub async fn ask_owner_checked(&self, question: &str) -> anyhow::Result<String> {
        let inlet = self.st.lock().telegram.clone();
        let chat = inlet.as_ref().and_then(|i| i.chat_id());
        let (Some(inlet), Some(chat)) = (inlet, chat) else {
            anyhow::bail!("no Telegram chat to ask the owner in");
        };
        if inlet.awaiting_answer() {
            // Another question is waiting (a task's, a Claude permission): asking now would take its answer.
            anyhow::bail!("another question is already waiting for the owner");
        }
        Ok(inlet.ask_user(question, chat, "Chrome access").await)
    }

    /// `_ask_owner_on_phone(question)`: the owner's answer through the text door, "" when there is no door or
    /// no chat yet.
    ///
    /// A yes/no for the owner over Telegram (chrome_consent.py). Where Python raised (no way to ask, or another
    /// question waiting) this is "" with the reason logged; [`Daemon::ask_owner_checked`] keeps the error.
    pub async fn ask_owner_on_phone(&self, question: &str) -> String {
        match self.ask_owner_checked(question).await {
            Ok(answer) => answer,
            Err(e) => {
                tracing::info!("chrome access: not asked ({e})");
                String::new()
            }
        }
    }

    /// `_tell_owner_on_phone(text)`: a line to the owner through the text door, when there is one.
    pub async fn tell_owner_on_phone(&self, text: &str) {
        let inlet = self.st.lock().telegram.clone();
        let chat = inlet.as_ref().and_then(|i| i.chat_id());
        if let (Some(inlet), Some(chat)) = (inlet, chat) {
            inlet.say(chat, text).await;
        }
    }

    /// `_start_miniapp(tasks)`: the "Ask Claude" Mini App (EVE_MINIAPP=1) with its calls, Eve Link's panels, the
    /// apps' cards, the Work sessions and the phone's expressions; its jobs are pushed onto `tasks`.
    pub fn start_miniapp(&self, tasks: &mut Vec<Job>) {
        let d = self.me();
        let cfg = eve_brain::miniapp::configured(self.environ());
        if !cfg.enabled {
            return;
        }
        // Python checked here that the anthropic SDK is installed; the Rust Mini App calls the API itself.
        let miniapp =
            eve_brain::miniapp::MiniApp::new(&cfg, None, None, None, None, None, eve_brain::seams::Lent::FromEnv, None);
        self.st.lock().miniapp = Some(miniapp.clone());
        // "Call eve" in the Mini App (phone_call.py): push to talk into the Telegram chat's own brain, every
        // reply read back. It needs the chat (the brain) and the OpenRouter key (the ears and the voice).
        let server = miniapp.server.clone();
        // eve's apps (app_registry.APPS): the Mini App's Lights, Music and TV cards and Eve Link's Apps, each
        // the hub the chat and the voice share, or None when it is off or not set up on this Mac
        for app in eve_app_registry::APPS.iter() {
            let hub = match app.slot.as_str() {
                "lights" => self.lights_hub(),
                "spotify" => self.spotify_hub(),
                "firetv" => self.firetv_hub(),
                _ => self.app_hub(app),
            };
            match app.slot.as_str() {
                "lights" => *server.lights.lock() = hub.and_then(|h| h.link_hub()),
                "spotify" => *server.spotify.lock() = hub.and_then(|h| h.link_hub()),
                "firetv" => *server.firetv.lock() = hub.and_then(|h| h.link_hub()),
                "pika" => *server.pika.lock() = hub.and_then(|h| h.panel()),
                "obsbot" => *server.obsbot.lock() = hub.and_then(|h| h.panel()),
                slot => {
                    let mut cards = server.cards.lock();
                    match hub.and_then(|h| h.link_hub()) {
                        Some(h) => {
                            cards.insert(slot.to_string(), h);
                        }
                        None => {
                            cards.remove(slot);
                        }
                    }
                }
            }
        }
        // the Memory card (link_memory.py): the memory is built after this, on a thread, so it is read per call
        let memory_panel = {
            let d = d.clone();
            eve_link_memory::MemoryPanel::new(move || {
                d.st.lock().memory.clone().map(|m| Arc::new(LentMemory(m)) as Arc<dyn eve_link_memory::MemoryApi>)
            })
        };
        // Notes, the owner's vault (link_notes.py, second_brain.py)
        {
            let mut cards = server.cards.lock();
            cards.insert("memory".into(), Arc::new(MemoryCard(memory_panel)) as Arc<dyn eve_link::seams::Hub>);
            cards.insert(
                "notes".into(),
                Arc::new(NotesCard(eve_link_notes::NotesPanel::default())) as Arc<dyn eve_link::seams::Hub>,
            );
        }
        let (door, real) = {
            let st = self.st.lock();
            (st.telegram.clone(), st.telegram_inlet.clone())
        };
        let check_owner: eve_phone_call::CheckOwner = {
            let (token, owners) = (cfg.token.clone(), cfg.owner_ids.clone());
            Arc::new(move |init_data: &str| {
                eve_brain::miniapp::check_init_data(
                    init_data,
                    &token,
                    &owners,
                    None,
                    eve_brain::miniapp::INIT_DATA_MAX_AGE_SECS,
                )
            })
        };
        let voice = eve_phone_call::make_voice(None).map(|v| Arc::new(v) as Arc<dyn eve_phone_call::Voice>);
        let calls = Arc::new(eve_phone_call::PhoneCalls::new(
            check_owner,
            real.clone().map(|i| i as Arc<dyn eve_phone_call::Brain>),
            voice,
        ));
        *server.calls.lock() = Some(calls.clone());
        // Eve Link's chat (link_chat.py): the owner texts the chat's brain from the app, with the link token.
        // The Work tab's Claude Code sessions in ~/Documents/personal/work (work_sessions.py,
        // docs/work-sessions.md); EVE_WORK_SESSIONS=0 leaves them off and the routes answer 503.
        if eve_work_sessions::enabled(self.environ()) {
            let d = d.clone();
            let state: eve_work_sessions::StateFn =
                Arc::new(move || Some(Arc::new(parking_lot::Mutex::new(state_view(&d)))));
            let work = eve_work_sessions::WorkSessions::new(eve_work_sessions::Options {
                state,
                environ: self.environ.clone(),
                ..Default::default()
            });
            self.st.lock().work = Some(Arc::new(work));
        }
        let inlet_fn: eve_link::seams::InletFn = {
            let d = d.clone();
            Arc::new(move || d.st.lock().telegram_inlet.clone().map(|i| i as Arc<dyn eve_link::seams::Inlet>))
        };
        let work_fn: eve_link::seams::WorkFn = {
            let d = d.clone();
            Arc::new(move || d.st.lock().work.clone().map(|w| w as Arc<dyn eve_link::seams::WorkDoor>))
        };
        let link = Arc::new(eve_link::link_chat::LinkChat::new(
            inlet_fn,
            eve_link::link_chat::door::LinkChatOptions { work: Some(work_fn), ..Default::default() },
        ));
        *server.link.lock() = Some(link.clone());
        // Notes on the phone (owner, 2026-10-04: "the phone should hold the port until i click action button
        // to turn it off, this allows me to take notes on the go"): take_notes said on an Eve Link call records
        // the phone through the room's own note-taker, and the notes are texted through the chat's door.
        if let Some(door) = &door {
            if let Some(inlet) = &real {
                // eve's own pictures and files are kept in the app door's store and shown in its chat, from the
                // start, not only once the app has called (TelegramInlet._send_media)
                *inlet.link_uploads.lock() = Some(link.uploads().clone());
            }
            calls.set_notes(Some(d.phone_notes(door)));
            if let Some(inlet) = &real {
                *inlet.phone_notes.lock() = Some(phone_notes_action(&calls));
            }
        }
        self.start_phone_expressions(&calls, tasks);
        let running = miniapp.clone();
        tasks.push(Job::spawn("miniapp", async move { running.run().await }));
        if let Some(inlet) = real {
            // "make me a habit tracker" texted to eve builds an app (apps_maker.ChatMaker); its Open button
            // is sent to the chat when the build is done.
            let spawner = Arc::downgrade(&inlet);
            let spawn: eve_brain::seams::SpawnFn = Arc::new(move |coro: BoxFuture<'static, ()>, name: &str| {
                if let Some(inlet) = spawner.upgrade() {
                    inlet.door_spawn(coro, name);
                }
            });
            // One set of apps-being-changed for both doors, so a delete, rename or undo from either one waits
            // for a build started from the other. (ChatMaker keeps a set of its own here: see PARITY.md.)
            let maker =
                eve_brain::apps_maker::ChatMaker::new(miniapp.clone() as Arc<dyn eve_brain::seams::MakerHost>, spawn);
            *inlet.maker.lock() = Some(maker);
        }
    }

    /// `_start_phone_expressions(calls, tasks)`: the phone's expression picker, lent the Mini App's calls.
    ///
    /// eve's expressions in the phone's eyes (owner, 2026-10-12: "route the expressions into the eyes"): a picker of
    /// its own for Eve Link's voice mode, fed by the call's turns and sending only to a live call whose app can draw
    /// them. On unless EVE_PHONE_EXPRESSIONS=0; the board's switch file is not involved.
    pub fn start_phone_expressions(&self, calls: &Arc<eve_phone_call::PhoneCalls>, tasks: &mut Vec<Job>) {
        let on = match self.environ() {
            Some(env) => eve_live_expressions::phone_expressions_on(env),
            None => eve_live_expressions::phone_expressions_on(&eve_jev::ProcessEnv),
        };
        if !on {
            tracing::info!("expressions: the phone's eyes are off (EVE_PHONE_EXPRESSIONS)");
            return;
        }
        let send = {
            let calls = calls.clone();
            eve_live_expressions::send_fn(move |cmd: Value| {
                let calls = calls.clone();
                async move { Ok(calls.send_expression(&cmd).await) }
            })
        };
        let connected: eve_live_expressions::Flag = {
            let calls = calls.clone();
            Arc::new(move || calls.eyes_live())
        };
        let phase: eve_live_expressions::PhaseFn = {
            let calls = calls.clone();
            Arc::new(move || if calls.active().is_some_and(|c| c.speaking()) { "speaking" } else { "idle" }.to_string())
        };
        let env = self.environ.clone().map(|e| Arc::new(e) as Arc<dyn eve_jev::Env + Send + Sync>);
        let service = Arc::new(eve_live_expressions::LiveExpressions::new(
            send,
            eve_live_expressions::Options {
                connected,
                phase,
                enabled: Some(true),
                lazy: true,
                env,
                ..Default::default()
            },
        ));
        self.st.lock().phone_expressions = Some(service.clone());
        let offer = service.clone();
        calls.set_on_text(Some(Arc::new(move |who: &str, text: &str| Ok(offer.offer(who, text).map(i64::from)))));
        tasks.push(Job::spawn("phone-expressions", service.run()));
    }
}

/// `inlet._phone_notes = calls.notes_action`: take_notes asks the phone first; None is the room note-taker's turn.
fn phone_notes_action(calls: &Arc<eve_phone_call::PhoneCalls>) -> eve_brain::seams::PhoneNotes {
    let calls = calls.clone();
    Arc::new(move |action: String, call_say: Option<eve_brain::seams::Reader>, later: eve_brain::seams::Later| {
        let calls = calls.clone();
        async move {
            let later: eve_phone_call::Later = Box::new(move |f| later(f));
            let out = calls.notes_action(&action, call_say.as_ref(), "asked from Telegram", Some(later)).await?;
            Some(match out {
                Value::Object(m) => m,
                other => {
                    let mut m = Obj::new();
                    m.insert("result".into(), other);
                    m
                }
            })
        }
        .boxed()
    })
}

/// `lambda: self.state`, as the Work sessions read it: what the daemon's state holds now. The daemon keeps its state
/// in a lock of its own, so the Work sessions see a copy of its sessions taken per call (see PARITY.md).
fn state_view(d: &Daemon) -> eve_state::State {
    let st = d.state.lock();
    let mut view = eve_state::State::new();
    view.sessions = st.sessions.clone();
    view.entries = st.entries.clone();
    view.tokens_cumulative = st.tokens_cumulative;
    view.tokens_today = st.tokens_today;
    view.cost_cumulative = st.cost_cumulative;
    view.cost_today = st.cost_today;
    view.completed_until = st.completed_until;
    view
}

// ---- adapters: the daemon's objects as the door's traits --------------------------------------------------------

/// The claude_live picker's view of `self.state`: each call takes the daemon's state lock for itself, so nothing
/// holds it across the probe's await.
struct StateBook(Arc<Daemon>);

impl eve_claude_live::SessionBook for StateBook {
    fn known_sessions(&self) -> Vec<eve_claude_live::KnownSession> {
        eve_claude_live::SessionBook::known_sessions(&*self.0.state.lock())
    }
    fn session_end(&mut self, session_id: &str) {
        self.0.state.lock().session_end(session_id);
    }
}

/// `self._scene` as the chat's look / look_around / find / move_head take it (scene.SceneWatcher).
pub struct LentScene(pub Arc<eve_scene::SceneWatcher>);

#[async_trait::async_trait]
impl eve_head::Scene for LentScene {
    async fn look(&self, fresh_secs: f64, newer_than: Option<f64>) -> Value {
        Value::Object(self.0.look(Some(fresh_secs), newer_than).await)
    }
    async fn locate(&self, target: &str, newer_than: Option<f64>) -> Value {
        Value::Object(self.0.locate(target, newer_than).await)
    }
}

#[async_trait::async_trait]
impl eve_brain::seams::SceneWatch for LentScene {
    fn start(&self) {
        self.0.start(true);
    }
    async fn stop(&self) {
        self.0.stop().await;
    }
}

/// `self._room_notes_taker()` as the chat's take_notes drives it (notes.RoomNotes).
pub struct LentNotes(pub Arc<eve_notes::RoomNotes>);

#[async_trait::async_trait]
impl eve_brain::seams::RoomNotes for LentNotes {
    fn start(&self) -> Value {
        self.0.start()
    }
    async fn stop(&self, reason: &str) -> Value {
        self.0.stop(reason).await
    }
    fn status(&self) -> Obj {
        match self.0.status() {
            Value::Object(m) => m,
            _ => Obj::new(),
        }
    }
}

/// memory.Memory as the Memory card calls it (link_memory.py).
pub struct LentMemory(pub Arc<eve_memory::Memory>);

impl eve_link_memory::MemoryApi for LentMemory {
    fn profile(&self) -> anyhow::Result<Option<String>> {
        Ok(Some(self.0.profile()))
    }
    fn search(&self, query: &str) -> anyhow::Result<Value> {
        Ok(self.0.search(query, None))
    }
    fn forget_preview(&self, query: &str) -> anyhow::Result<Value> {
        Ok(self.0.forget_preview(query))
    }
    fn forget_apply(&self, token: &str) -> anyhow::Result<Value> {
        Ok(self.0.forget_apply(token))
    }
    fn star(&self, text: &str) -> anyhow::Result<Option<String>> {
        Ok(self.0.star(text))
    }
    fn hide(&self, text: &str) -> anyhow::Result<Option<String>> {
        Ok(self.0.hide(text))
    }
    fn unhide(&self, text: &str) -> anyhow::Result<bool> {
        Ok(self.0.unhide(text))
    }
    fn hidden(&self) -> anyhow::Result<Vec<String>> {
        Ok(self.0.hidden())
    }
}

/// `server.memory = MemoryPanel(...)`: the Memory card as Eve Link asks for it by name.
pub struct MemoryCard(pub eve_link_memory::MemoryPanel);

#[async_trait::async_trait]
impl eve_link::seams::Hub for MemoryCard {
    fn available(&self) -> bool {
        self.0.available()
    }
    async fn panel(&self, body: &Value) -> anyhow::Result<Value> {
        self.0.panel(body).await
    }
}

/// `server.notes = NotesPanel()`: the Notes app as Eve Link asks for it by name.
pub struct NotesCard(pub eve_link_notes::NotesPanel);

#[async_trait::async_trait]
impl eve_link::seams::Hub for NotesCard {
    fn available(&self) -> bool {
        self.0.available()
    }
    async fn panel(&self, body: &Value) -> anyhow::Result<Value> {
        let body = match body {
            Value::Object(m) => m.clone(),
            _ => Map::new(),
        };
        self.0.panel(&body).await.map_err(anyhow::Error::new)
    }
}

/// The direct Calendar (apps_mcp.AppsBridge.execute) as the Meet notetaker lists the owner's events: called from a
/// worker thread (the Meeter's `to_thread`), so it waits on the async call there.
pub struct DirectCalendar(pub Arc<eve_apps_mcp::AppsBridge>);

impl eve_meet::CalendarApps for DirectCalendar {
    fn execute(&self, tool: &str, args: Value) -> anyhow::Result<Value> {
        let args = match args {
            Value::Object(m) => m,
            _ => Map::new(),
        };
        let bridge = self.0.clone();
        let tool = tool.to_string();
        let call = async move { Value::Object(bridge.execute(&tool, &args).await) };
        Ok(match tokio::runtime::Handle::try_current() {
            Ok(handle) => handle.block_on(call),
            Err(_) => tokio::runtime::Builder::new_current_thread().enable_all().build()?.block_on(call),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_policy_is_logged_as_pythons_dict_repr() {
        let mut policy = indexmap::IndexMap::new();
        policy.insert("gmail".to_string(), "read".to_string());
        policy.insert("it's".to_string(), "ask".to_string());
        assert_eq!(py_repr_policy(&policy), "{'gmail': 'read', \"it's\": 'ask'}");
        assert_eq!(py_repr_policy(&indexmap::IndexMap::new()), "{}");
    }

    #[test]
    fn an_executor_error_keeps_the_chiefs_own_name() {
        let r = raised(anyhow::Error::new(eve_chief::Raised::new("RuntimeError", "no chat")));
        assert_eq!((r.name.as_str(), r.message.as_str()), ("RuntimeError", "no chat"));
        let r = raised(anyhow::anyhow!("boom"));
        assert_eq!((r.name.as_str(), r.message.as_str()), ("Exception", "boom"));
    }
}
