//! eve's apps' hubs (`_app_hub`, `_app_hubs`, `_lights_hub`, `_spotify_hub`, `_firetv_hub`) and the Voice PE's
//! say (`_board_say`). Each app is made once and kept in `DaemonState::hubs` under its slot; a slot present with
//! None is an app that is off this run (Python's `hasattr` with None).
//!
//! Python handed each app crate's hub object to the chat, the voice and the Mini App as it was (duck typing). Here
//! each one is wrapped once ([`LightsHub`], [`SpotifyHub`], [`FireTvHub`], [`ObsbotHub`], [`PikaHub`]) in the faces
//! those sites take: [`eve_brain::seams::AppHub`] (the models' tools and the code path),
//! [`eve_link::seams::Hub`] (the Mini App's card), [`eve_brain::seams::PanelCard`] (the camera's and the car's
//! panels), and [`DaemonHub`] (the daemon's own handle on it, with its close). The module-level side Python read
//! with `app.mod()` (its own `match`, `NOT_SET_UP_LINE`, `enabled()`) is [`AppModuleOf`].

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};

use crate::daemon::Daemon;
use crate::seams::{DaemonHub, Environ, Obj};
use eve_brain::seams::{AppHub, AppHubs, AppModule, HubCommand, PanelCard};
use eve_link::seams::{DynWrite, Hub};

/// `make_lights()`'s hub as a [`DaemonHub`].
pub struct LightsHub(pub Arc<eve_lights::Lights>);
/// `make_spotify()`'s hub.
pub struct SpotifyHub(pub Arc<eve_spotify::Spotify>);
/// `make_firetv()`'s hub.
pub struct FireTvHub(pub Arc<eve_firetv::FireTV>);
/// The OBSBOT camera's panel (`make_panel`).
pub struct ObsbotHub(pub Arc<eve_obsbot_panel::ObsbotPanel>);
/// The car (`make_pika`).
pub struct PikaHub(pub Arc<eve_pika::Pika>);

/// The Voice PE's say: `(text) -> spoken on the controller's speaker`.
pub type BoardSay = Arc<dyn Fn(String) -> futures::future::BoxFuture<'static, ()> + Send + Sync>;

/// The same say with its failure kept (Python's `say` raised to its caller): what Spotify mode is lent.
pub type BoardSayResult = Arc<dyn Fn(String) -> futures::future::BoxFuture<'static, anyhow::Result<()>> + Send + Sync>;

// ---- the command each code path hands back ------------------------------------------------------------------------

/// A code-path command as the chat keeps it: the app's own `Command`, with `action` and `query` read off it as
/// Python's `getattr(cmd, "action", None)` / `getattr(cmd, "query", None)` did.
fn command<C: Send + Sync + 'static>(cmd: C, action: Option<String>, query: Option<String>) -> HubCommand {
    HubCommand { action, query, inner: Arc::new(cmd) }
}

fn lights_command(cmd: eve_lights::Command) -> HubCommand {
    // the lights' Command has neither attribute
    command(cmd, None, None)
}

fn spotify_command(cmd: eve_spotify::Command) -> HubCommand {
    let (action, query) = (cmd.action.clone(), cmd.query.clone());
    command(cmd, Some(action), Some(query))
}

fn firetv_command(cmd: eve_firetv::Command) -> HubCommand {
    let (action, query) = (cmd.action.clone(), cmd.query.clone());
    command(cmd, Some(action), Some(query))
}

fn obsbot_command(cmd: eve_obsbot_panel::Command) -> HubCommand {
    // the camera's Command has an action and no query
    let action = cmd.action.clone();
    command(cmd, Some(action), None)
}

/// A failure Python would have raised out of `handle` (the chat's turn then failed): kept as the tool's result, so
/// the model reads that it failed.
fn failed(e: &dyn std::fmt::Display) -> Value {
    json!({"ok": false, "reason": e.to_string()})
}

fn as_obj(v: &Value) -> Obj {
    v.as_object().cloned().unwrap_or_default()
}

// ---- the lights ---------------------------------------------------------------------------------------------------

#[async_trait]
impl AppHub for LightsHub {
    fn tools(&self) -> Vec<Value> {
        self.0.tools()
    }
    fn instructions(&self, _kwargs: &Obj) -> String {
        self.0.instructions()
    }
    async fn handle(&self, name: &str, args: &Value) -> Value {
        self.0.handle(name, args).await
    }
    fn match_command(&self, text: &str) -> Option<HubCommand> {
        self.0.match_command(text).map(lights_command)
    }
    async fn run(&self, cmd: &HubCommand) -> String {
        match cmd.inner.downcast_ref::<eve_lights::Command>() {
            Some(c) => self.0.run(c).await,
            None => String::new(),
        }
    }
}

#[async_trait]
impl Hub for LightsHub {
    fn listing(&self) -> Vec<Value> {
        self.0.listing()
    }
    async fn panel(&self, body: &Value) -> anyhow::Result<Value> {
        Ok(self.0.panel(body).await)
    }
}

#[async_trait]
impl DaemonHub for LightsHub {
    fn slot(&self) -> String {
        "lights".into()
    }
    fn app_hub(&self) -> Option<Arc<dyn AppHub>> {
        Some(Arc::new(LightsHub(self.0.clone())))
    }
    fn link_hub(&self) -> Option<Arc<dyn Hub>> {
        Some(Arc::new(LightsHub(self.0.clone())))
    }
    async fn close(&self) {
        self.0.close().await
    }
}

// ---- Spotify ------------------------------------------------------------------------------------------------------

#[async_trait]
impl AppHub for SpotifyHub {
    fn tools(&self) -> Vec<Value> {
        self.0.tools()
    }
    fn instructions(&self, _kwargs: &Obj) -> String {
        self.0.instructions().to_string()
    }
    async fn handle(&self, name: &str, args: &Value) -> Value {
        match self.0.handle(name, &as_obj(args)).await {
            Ok(out) => Value::Object(out),
            Err(e) => failed(&e),
        }
    }
    fn match_command(&self, text: &str) -> Option<HubCommand> {
        self.0.match_text(text).map(spotify_command)
    }
    async fn run(&self, cmd: &HubCommand) -> String {
        let Some(c) = cmd.inner.downcast_ref::<eve_spotify::Command>() else {
            return String::new();
        };
        match self.0.run(c).await {
            Ok(line) => line,
            Err(e) => {
                tracing::warn!("spotify: by code, {} failed ({e})", c.action);
                String::new()
            }
        }
    }
}

#[async_trait]
impl Hub for SpotifyHub {
    async fn panel(&self, body: &Value) -> anyhow::Result<Value> {
        Ok(Value::Object(self.0.panel(&as_obj(body)).await?))
    }
}

#[async_trait]
impl DaemonHub for SpotifyHub {
    fn slot(&self) -> String {
        "spotify".into()
    }
    fn app_hub(&self) -> Option<Arc<dyn AppHub>> {
        Some(Arc::new(SpotifyHub(self.0.clone())))
    }
    fn link_hub(&self) -> Option<Arc<dyn Hub>> {
        Some(Arc::new(SpotifyHub(self.0.clone())))
    }
    async fn close(&self) {
        self.0.close().await
    }
}

// ---- the TV -------------------------------------------------------------------------------------------------------

#[async_trait]
impl AppHub for FireTvHub {
    fn tools(&self) -> Vec<Value> {
        self.0.tools()
    }
    /// `instructions(screenshot=True)`: the voice passes `screenshot=False` (its `voice_kwargs`).
    fn instructions(&self, kwargs: &Obj) -> String {
        let screenshot = kwargs.get("screenshot").is_none_or(truthy);
        self.0.instructions(screenshot).to_string()
    }
    async fn handle(&self, name: &str, args: &Value) -> Value {
        match self.0.handle(name, args).await {
            Ok(out) => Value::Object(out),
            Err(e) => failed(&e),
        }
    }
    fn match_command(&self, text: &str) -> Option<HubCommand> {
        self.0.match_command(text).map(firetv_command)
    }
    async fn run(&self, cmd: &HubCommand) -> String {
        match cmd.inner.downcast_ref::<eve_firetv::Command>() {
            Some(c) => self.0.run(c).await,
            None => String::new(),
        }
    }
    async fn screenshot_for_model(&self) -> Option<Value> {
        Some(match self.0.screenshot_for_model().await {
            Ok(out) => Value::Object(out),
            Err(e) => failed(&e),
        })
    }
}

/// Python's truthiness of a keyword argument.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

#[async_trait]
impl Hub for FireTvHub {
    async fn panel(&self, body: &Value) -> anyhow::Result<Value> {
        Ok(Value::Object(self.0.panel(body).await))
    }
}

#[async_trait]
impl DaemonHub for FireTvHub {
    fn slot(&self) -> String {
        "firetv".into()
    }
    fn app_hub(&self) -> Option<Arc<dyn AppHub>> {
        Some(Arc::new(FireTvHub(self.0.clone())))
    }
    fn link_hub(&self) -> Option<Arc<dyn Hub>> {
        Some(Arc::new(FireTvHub(self.0.clone())))
    }
    async fn close(&self) {
        self.0.close().await
    }
}

// ---- the camera ---------------------------------------------------------------------------------------------------

#[async_trait]
impl AppHub for ObsbotHub {
    fn tools(&self) -> Vec<Value> {
        self.0.tools()
    }
    fn instructions(&self, _kwargs: &Obj) -> String {
        self.0.instructions()
    }
    async fn handle(&self, name: &str, args: &Value) -> Value {
        self.0.handle(name, args).await
    }
    fn match_command(&self, text: &str) -> Option<HubCommand> {
        self.0.r#match(text).map(obsbot_command)
    }
    async fn run(&self, cmd: &HubCommand) -> String {
        match cmd.inner.downcast_ref::<eve_obsbot_panel::Command>() {
            Some(c) => self.0.run(c).await,
            None => String::new(),
        }
    }
}

#[async_trait]
impl Hub for ObsbotHub {
    fn available(&self) -> bool {
        self.0.available()
    }
    async fn panel(&self, body: &Value) -> anyhow::Result<Value> {
        Ok(self.0.panel(body).await)
    }
    async fn stream(&self, writer: &mut DynWrite) -> std::io::Result<()> {
        let mut w = writer;
        self.0.stream(&mut w).await;
        Ok(())
    }
}

#[async_trait]
impl PanelCard for ObsbotHub {
    async fn close(&self) {
        self.0.close().await
    }
}

#[async_trait]
impl DaemonHub for ObsbotHub {
    fn slot(&self) -> String {
        "obsbot".into()
    }
    fn app_hub(&self) -> Option<Arc<dyn AppHub>> {
        Some(Arc::new(ObsbotHub(self.0.clone())))
    }
    fn panel(&self) -> Option<Arc<dyn PanelCard>> {
        Some(Arc::new(ObsbotHub(self.0.clone())))
    }
    async fn close(&self) {
        self.0.close().await
    }
}

// ---- the car ------------------------------------------------------------------------------------------------------

#[async_trait]
impl Hub for PikaHub {
    // `Pika.available = True`: the car's card is there whenever the car is set up
    async fn panel(&self, body: &Value) -> anyhow::Result<Value> {
        Ok(self.0.panel(body).await)
    }
    async fn stream(&self, writer: &mut DynWrite) -> std::io::Result<()> {
        let mut w = writer;
        self.0.stream(&mut w).await;
        Ok(())
    }
}

#[async_trait]
impl PanelCard for PikaHub {
    async fn close(&self) {
        self.0.close("closed").await
    }
}

#[async_trait]
impl DaemonHub for PikaHub {
    fn slot(&self) -> String {
        "pika".into()
    }
    /// The car gives the models no tools (`tools=False` in the registry).
    fn app_hub(&self) -> Option<Arc<dyn AppHub>> {
        None
    }
    fn panel(&self) -> Option<Arc<dyn PanelCard>> {
        Some(Arc::new(PikaHub(self.0.clone())))
    }
    async fn close(&self) {
        self.0.close("closed").await
    }
}

// ---- each app's module, as `app.mod()` read it ----------------------------------------------------------------------

/// An app's module-level side: its own `match` (one that needs no hub, so a switched-on app that is not set up can
/// say so), its `NOT_SET_UP_LINE`, and its `enabled()` read from the daemon's environment.
pub struct AppModuleOf {
    pub slot: String,
    pub environ: Option<Environ>,
}

impl AppModule for AppModuleOf {
    fn has_match(&self) -> bool {
        // lights.py and pika.py have no module-level match: the lights match their names on the hub
        matches!(self.slot.as_str(), "spotify" | "firetv" | "obsbot")
    }
    fn module_match(&self, text: &str) -> Option<HubCommand> {
        match self.slot.as_str() {
            "spotify" => eve_spotify::match_text(text).map(spotify_command),
            "firetv" => eve_firetv::match_command(text).map(firetv_command),
            "obsbot" => eve_obsbot_panel::r#match(text).map(obsbot_command),
            _ => None,
        }
    }
    fn not_set_up_line(&self) -> Option<String> {
        match self.slot.as_str() {
            "spotify" => Some(eve_spotify::NOT_SET_UP_LINE.to_string()),
            "firetv" => Some(eve_firetv::NOT_SET_UP_LINE.to_string()),
            "obsbot" => Some(eve_obsbot_panel::NOT_SET_UP_LINE.to_string()),
            _ => None,
        }
    }
    fn enabled(&self) -> bool {
        let env = self.environ.as_ref();
        match self.slot.as_str() {
            "lights" => eve_lights::enabled(env),
            "spotify" => eve_spotify::enabled(env),
            "firetv" => eve_firetv::enabled(env),
            "obsbot" => eve_obsbot_panel::enabled(env),
            "pika" => eve_pika::pika::enabled(env),
            _ => true,
        }
    }
}

/// `getattr(app.mod(), app.make)()`: the app's factory by slot, reading its own switch and files. None: switched
/// off or not set up here.
pub fn make_hub(slot: &str, environ: Option<&Environ>) -> Option<Arc<dyn DaemonHub>> {
    made(slot, environ).0
}

/// [`make_hub`], with Spotify's own hub beside it (Spotify mode is lent it).
fn made(slot: &str, environ: Option<&Environ>) -> (Option<Arc<dyn DaemonHub>>, Option<Arc<eve_spotify::Spotify>>) {
    if slot == "spotify" {
        let spotify = eve_spotify::make_spotify(None, environ, None).map(Arc::new);
        let hub = spotify.clone().map(|h| Arc::new(SpotifyHub(h)) as Arc<dyn DaemonHub>);
        return (hub, spotify);
    }
    let hub = match slot {
        "lights" => eve_lights::make_lights(None, environ).map(|h| Arc::new(LightsHub(Arc::new(h))) as Arc<dyn DaemonHub>),
        "firetv" => eve_firetv::make_firetv(None, environ, None, None)
            .map(|h| Arc::new(FireTvHub(Arc::new(h))) as Arc<dyn DaemonHub>),
        "obsbot" => eve_obsbot_panel::make_panel(environ).map(|h| Arc::new(ObsbotHub(Arc::new(h))) as Arc<dyn DaemonHub>),
        "pika" => eve_pika::make_pika(None, environ).map(|h| Arc::new(PikaHub(Arc::new(h))) as Arc<dyn DaemonHub>),
        _ => None,
    };
    (hub, None)
}

fn panic_text(p: &(dyn std::any::Any + Send)) -> String {
    p.downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| p.downcast_ref::<String>().cloned())
        .unwrap_or_default()
}

impl Daemon {
    /// `_app_hub(app)`: the app's hub, made on the first ask (app.make, by slot), None when it is off.
    ///
    /// One of eve's apps (app_registry.APPS), made once and shared by the chat, the voice, the Mini App and Eve
    /// Link; None when it is switched off or not set up (no lights file, no login, never paired, no camera). The
    /// module's factory reads its own switch and files at the first ask, so a restart picks up a new scan. Making
    /// one sends nothing to its device: the first command connects.
    pub fn app_hub(&self, app: &eve_app_registry::App) -> Option<Arc<dyn DaemonHub>> {
        if let Some(made) = self.st.lock().hubs.get(&app.slot) {
            return made.clone();
        }
        let environ = self.environ().cloned();
        let slot = app.slot.clone();
        let (hub, spotify) = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| made(&slot, environ.as_ref()))) {
            Ok(made) => made,
            Err(p) => {
                // an app that cannot load costs that app, never the daemon
                tracing::error!("{}: could not load; it is off this run ({})", app.slot, panic_text(p.as_ref()));
                (None, None)
            }
        };
        {
            let mut st = self.st.lock();
            if let Some(made) = st.hubs.get(&app.slot) {
                // made meanwhile by another caller: the first one stands
                return made.clone();
            }
            st.hubs.insert(app.slot.clone(), hub.clone());
        }
        if app.slot == "spotify" {
            if let (Some(spotify), Some(ctl)) = (spotify, self.controller.clone()) {
                {
                    // Spotify mode on the Voice PE: its ring green, its dial the volume, its clicks the transport
                    let send: eve_music_mode::SendFn = Arc::new(move |obj: Value| {
                        let ctl = ctl.clone();
                        Box::pin(async move {
                            ctl.show(&obj).await;
                            Ok(())
                        })
                    });
                    let say: Option<eve_music_mode::SayFn> = self.board_say_result();
                    let mode = Arc::new(eve_music_mode::MusicMode::new(spotify.clone(), send, say));
                    self.st.lock().music_mode = Some(mode.clone());
                    spotify.set_mode(Some(mode as Arc<dyn eve_spotify::SpotifyMode>));
                }
            }
        }
        hub
    }

    /// `_app_hubs(tools_only)`: every app's hub keyed by its slot (e.g. "lights"), for the chat and the voice;
    /// `tools_only`: only the apps that give tools.
    ///
    /// Every app's hub by slot, in tool order (app_registry.APPS); ``tools_only``: the apps that give the models
    /// tools (the chat's and the voice's kwargs). The lights', Spotify's and the TV's own methods are called for
    /// those three, as Python's `_<slot>_hub`. Each app's module (its own `match`, `NOT_SET_UP_LINE`, `enabled`) is
    /// lent beside its hub, as Python's sites read `app.mod()`.
    pub fn app_hubs(&self, tools_only: bool) -> AppHubs {
        let apps: Vec<&'static eve_app_registry::App> =
            if tools_only { eve_app_registry::with_tools() } else { eve_app_registry::APPS.iter().collect() };
        let mut hubs: Vec<(String, Arc<dyn AppHub>)> = Vec::new();
        let mut modules: Vec<(String, Arc<dyn AppModule>)> = Vec::new();
        for app in apps {
            let made = match app.slot.as_str() {
                "lights" => self.lights_hub(),
                "spotify" => self.spotify_hub(),
                "firetv" => self.firetv_hub(),
                _ => self.app_hub(app),
            };
            if let Some(hub) = made.and_then(|h| h.app_hub()) {
                hubs.push((app.slot.clone(), hub));
            }
            modules.push((
                app.slot.clone(),
                Arc::new(AppModuleOf { slot: app.slot.clone(), environ: self.environ().cloned() }),
            ));
        }
        match AppHubs::new(hubs) {
            Ok(out) => out.with_modules(modules),
            Err(e) => {
                // every slot comes from the registry, so this is a registry out of step with itself
                tracing::error!("apps: {e}");
                AppHubs::default().with_modules(modules)
            }
        }
    }

    /// `_lights_hub()`.
    pub fn lights_hub(&self) -> Option<Arc<dyn DaemonHub>> {
        self.app_hub(eve_app_registry::get("lights")?)
    }

    /// `_spotify_hub()`.
    pub fn spotify_hub(&self) -> Option<Arc<dyn DaemonHub>> {
        self.app_hub(eve_app_registry::get("spotify")?)
    }

    /// `_firetv_hub()`.
    pub fn firetv_hub(&self) -> Option<Arc<dyn DaemonHub>> {
        self.app_hub(eve_app_registry::get("firetv")?)
    }

    /// `_board_say()`: speak on the Voice PE when one is attached, None otherwise.
    ///
    /// A few words on the Voice PE's speaker (the device picker's names), with the call voice; None without one (no
    /// OPENROUTER_API_KEY): the picker then shows the ring alone. A failure to speak is logged here: this say's
    /// callers wait for it and go on.
    pub fn board_say(&self) -> Option<BoardSay> {
        let say = self.board_say_result()?;
        Some(Arc::new(move |text: String| {
            let fut = say(text);
            Box::pin(async move {
                if let Err(e) = fut.await {
                    tracing::warn!("controller: could not say a name on the Voice PE ({e})");
                }
            })
        }))
    }

    /// `_board_say()` with the failure kept, for Spotify mode (`MusicMode(..., say=...)`).
    pub fn board_say_result(&self) -> Option<BoardSayResult> {
        let voice = Arc::new(eve_phone_call::make_voice(self.environ())?);
        let ctl = self.controller.clone();
        Some(Arc::new(move |text: String| {
            let voice = voice.clone();
            let ctl = ctl.clone();
            Box::pin(async move {
                let Some(ctl) = ctl.filter(|c| c.connected()) else {
                    return Ok(());
                };
                if text.is_empty() {
                    return Ok(());
                }
                let pcm = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
                    use eve_phone_call::Voice;
                    let mut pcm = Vec::new();
                    for chunk in voice.speak_stream(&text) {
                        pcm.extend(chunk?);
                    }
                    Ok(pcm)
                })
                .await??;
                let send: eve_desk_call::BoardSend = Arc::new(move |obj: Value| {
                    let ctl = ctl.clone();
                    Box::pin(async move { ctl.send_audio(&obj).await })
                });
                let speaker = eve_desk_call::BoardSpeaker::new(send);
                speaker.flush_board().await?; // a name not yet finished gives way to the next
                speaker.play_pcm(&pcm).await
            })
        }))
    }

    /// Close every app's hub made this run (shutdown): a held Bluetooth light goes back to the phone app, the car
    /// stops, the camera's picture ends (re-entrant: the Mini App closes some too).
    pub async fn close_hubs(&self) {
        for app in eve_app_registry::APPS.iter() {
            let hub = self.st.lock().hubs.get(&app.slot).cloned().flatten();
            if let Some(hub) = hub {
                hub.close().await;
            }
        }
    }
}
