//! eve's apps, declared once (owner, 2026-10-12: "make it so that apps are variable ... these are just the apps i
//! want"). Port of `bridge/src/eve_bridge/app_registry.py`.
//!
//! An app is a module plus one line here. The line says what the module offers and where it reaches: the chat
//! brain's tools and block (telegram), the voice backend's (voice_agent), the code path that answers a plain command
//! with no model turn, the tool gate's sets (tool_gate), the phone's Apps list and sheet (link_chat), the CLI (cli),
//! and the intent eval's labels (tools/intent_eval.py). Every site loops over [`APPS`]; none names an app.
//!
//! The apps below are the owner's picks, not a fixed set. `eve-bridge app new <name>` writes a new one (a module,
//! its test, its doc and a line above the marker at the end of `APPS`); docs/adding-an-app.md walks through it.
//!
//! **Order is model bytes.** [`APPS`] is in tool order: each app's tools and instruction block reach the chat and
//! the voice models in this order, so moving a line changes what the models read (AGENTS.md rule 6). A new app goes
//! last, and it is off by default (`default_on: false`), so adding one changes no byte until its switch is on.
//! [`CODE_ORDER`] is the code path's order and [`PHONE_ORDER`] the phone's; an app not named in them comes after the
//! named ones, in `APPS` order.
//!
//! This crate is import-light: strings and sets only. Python reached an app's module with `importlib` (`app.mod()`);
//! Rust has no runtime import, so each app crate describes its module with a [`ModuleInfo`] and the daemon registers
//! it ([`register_module`]). Functions that read a module take a [`ModuleSource`]; the plain forms read the
//! registered modules.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;
use parking_lot::RwLock;
use regex::Regex;
use serde_json::{Map, Value};

// ---- an app's module, as the registry reads it ----------------------------------------------------------------------

/// A class an app module defines (its hub), by name and callable members (`panel`, `close`, `match`, `run`,
/// `stream`, ...): enough for [`check`] to see the hub's methods without making one (no device is touched).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ClassInfo {
    pub name: String,
    pub methods: BTreeSet<String>,
}

impl ClassInfo {
    pub fn new(name: &str, methods: &[&str]) -> Self {
        Self { name: name.to_string(), methods: methods.iter().map(|m| m.to_string()).collect() }
    }

    fn has(&self, method: &str) -> bool {
        self.methods.contains(method)
    }
}

/// What `eve_bridge.<name>` offers the registry: the attributes Python's `check()`, `owner()` and `name_problem()`
/// read off the imported module.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModuleInfo {
    /// The module's name, as in `eve_bridge.<name>` (an [`App`]'s `module` or `cli_module`).
    pub name: String,
    /// Module-level callables: the factory (`make_lights`), `match`, `cli`, `enabled`, ...
    pub functions: BTreeSet<String>,
    /// The classes defined in the module itself (Python's `obj.__module__ == mod.__name__`).
    pub classes: Vec<ClassInfo>,
    /// A factory's return class, from its `Optional[X]` annotation.
    pub returns: IndexMap<String, ClassInfo>,
    /// `TOOL_NAMES`.
    pub tool_names: Option<Vec<String>>,
    /// `TOOLS`: the tool schemas (a JSON list of objects with a `name`).
    pub tools: Option<Value>,
    /// `OFF_REASON`.
    pub off_reason: Option<String>,
    /// `INSTRUCTIONS`.
    pub instructions: Option<String>,
    /// `INSTRUCTIONS_HEAD`, for an app whose block lists the owner's devices (the lights).
    pub instructions_head: Option<String>,
}

impl ModuleInfo {
    pub fn new(name: &str) -> Self {
        Self { name: name.to_string(), ..Self::default() }
    }

    fn callable(&self, name: &str) -> bool {
        self.functions.contains(name)
    }

    /// `TOOL_NAMES`, or none.
    pub fn tool_names(&self) -> &[String] {
        self.tool_names.as_deref().unwrap_or(&[])
    }
}

/// Where the registry finds an app's module (Python's `importlib.import_module`).
pub trait ModuleSource {
    fn module(&self, name: &str) -> Option<Arc<ModuleInfo>>;
}

/// A set of modules by name.
#[derive(Clone, Debug, Default)]
pub struct Modules {
    by_name: HashMap<String, Arc<ModuleInfo>>,
}

impl Modules {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, info: ModuleInfo) {
        self.by_name.insert(info.name.clone(), Arc::new(info));
    }

    pub fn with(mut self, info: ModuleInfo) -> Self {
        self.insert(info);
        self
    }
}

impl ModuleSource for Modules {
    fn module(&self, name: &str) -> Option<Arc<ModuleInfo>> {
        self.by_name.get(name).cloned()
    }
}

static REGISTERED: LazyLock<RwLock<Modules>> = LazyLock::new(|| RwLock::new(Modules::new()));

/// Make a module known to the registry (each app crate's description of itself, registered at startup).
pub fn register_module(info: ModuleInfo) {
    REGISTERED.write().insert(info);
}

/// The registered modules (the source the plain functions read).
#[derive(Clone, Copy, Debug, Default)]
pub struct Registered;

impl ModuleSource for Registered {
    fn module(&self, name: &str) -> Option<Arc<ModuleInfo>> {
        REGISTERED.read().module(name)
    }
}

// ---- the app --------------------------------------------------------------------------------------------------------

/// One app. `slot` is its name everywhere: the chat's `inlet._<slot>`, `VoiceSession.<slot>`, the Mini App server's
/// attribute, the `/api/link/controls` key and `POST /api/link/controls/<slot>`.
#[derive(Clone, Debug, PartialEq)]
pub struct App {
    pub slot: String,
    /// eve_bridge.<module>
    pub module: String,
    /// the module's factory: the hub, or None when it is off or not set up
    pub make: String,
    /// its env var, e.g. EVE_LIGHTS
    pub switch: String,
    /// true: on unless 0/false/no/off; false: on only when 1/true/yes/on
    pub default_on: bool,
    /// the phone row: its title,
    pub title: String,
    /// its SF Symbol,
    pub symbol: String,
    /// and its subtitle
    pub detail: String,
    /// its doc, repo-relative
    pub doc: String,
    /// the phone's 404 line: the app is not on this Mac
    pub no_control: String,
    /// it gives the models tools: hub.tools()/instructions()/handle(), module.TOOLS, TOOL_NAMES, OFF_REASON and
    /// INSTRUCTIONS (the intent eval lends TOOLS and INSTRUCTIONS; INSTRUCTIONS_HEAD for an app whose block lists
    /// the owner's devices, like the lights, which the eval names itself)
    pub tools: bool,
    /// its intent-eval label
    pub label: String,
    /// every tool name starts with it
    pub tool_prefix: String,
    /// tools that change something (tool_gate.ACTING)
    pub acting: BTreeSet<String>,
    /// reads that bring in no outside text (tool_gate.CLEAN)
    pub clean: BTreeSet<String>,
    /// a read's name in the owner's question
    pub sources: IndexMap<String, String>,
    /// an act's head out loud (tool_gate._SAY_HEADS)
    pub say: IndexMap<String, String>,
    /// actions answering in eve's own words only
    pub own_words: IndexMap<String, BTreeSet<String>>,
    /// tools not offered out loud
    pub voice_omit: BTreeSet<String>,
    /// hub.instructions(**voice_kwargs) out loud
    pub voice_kwargs: Map<String, Value>,
    /// a line after its voice block
    pub voice_note: String,
    /// a code path: the module's match(text) (else hub.match) and hub.run
    pub code: bool,
    /// the code path's line is outside words when tool_gate.taints(this, ...)
    pub code_taint_tool: String,
    /// hub.panel(body) answers POST /api/link/controls/<slot>
    pub controls: bool,
    /// GET /api/link/controls/<slot>/stream
    pub stream: bool,
    /// its legacy controls key is hub.listing() (a list), not a bool
    pub listing: bool,
    /// the phone has Swift for it; any other app gets the generic sheet
    pub native_sheet: bool,
    /// eve-bridge words that run cli_module.cli(argv)
    pub cli: Vec<String>,
    /// "" means module
    pub cli_module: String,
    pub cli_help: String,
}

impl Default for App {
    /// The dataclass defaults; the required fields are empty.
    fn default() -> Self {
        Self {
            slot: String::new(),
            module: String::new(),
            make: String::new(),
            switch: String::new(),
            default_on: false,
            title: String::new(),
            symbol: String::new(),
            detail: String::new(),
            doc: String::new(),
            no_control: String::new(),
            tools: true,
            label: String::new(),
            tool_prefix: String::new(),
            acting: BTreeSet::new(),
            clean: BTreeSet::new(),
            sources: IndexMap::new(),
            say: IndexMap::new(),
            own_words: IndexMap::new(),
            voice_omit: BTreeSet::new(),
            voice_kwargs: Map::new(),
            voice_note: String::new(),
            code: false,
            code_taint_tool: String::new(),
            controls: true,
            stream: false,
            listing: false,
            native_sheet: false,
            cli: Vec::new(),
            cli_module: String::new(),
            cli_help: String::new(),
        }
    }
}

impl App {
    /// The app's module (Python's `app.mod()`), from the registered modules; None when no app crate registered it.
    pub fn module_info(&self) -> Option<Arc<ModuleInfo>> {
        self.module_in(&Registered)
    }

    pub fn module_in(&self, src: &dyn ModuleSource) -> Option<Arc<ModuleInfo>> {
        src.module(&self.module)
    }

    /// The module whose `cli(argv)` runs this app's CLI words (Python's `app.cli_mod()`).
    pub fn cli_module_info(&self) -> Option<Arc<ModuleInfo>> {
        self.cli_module_in(&Registered)
    }

    pub fn cli_module_in(&self, src: &dyn ModuleSource) -> Option<Arc<ModuleInfo>> {
        src.module(self.cli_module_name())
    }

    /// `cli_module or module`.
    pub fn cli_module_name(&self) -> &str {
        if self.cli_module.is_empty() {
            &self.module
        } else {
            &self.cli_module
        }
    }
}

fn s(v: &str) -> String {
    v.to_string()
}

/// Python's `_fs(*names)`.
fn fs(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|n| n.to_string()).collect()
}

/// Python's `_map(**kv)` for string values.
fn map(kv: &[(&str, &str)]) -> IndexMap<String, String> {
    kv.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
}

/// The owner's apps, in TOOL order (the order their tools and blocks reach the models).
pub static APPS: LazyLock<Vec<App>> = LazyLock::new(|| {
    vec![
        App {
            slot: s("lights"), module: s("lights"), make: s("make_lights"), switch: s("EVE_LIGHTS"), default_on: true,
            title: s("Lights"), symbol: s("lightbulb"), detail: s(""), doc: s("docs/lights.md"),
            no_control: s("eve has no lights on this computer."),
            label: s("lights"), tool_prefix: s("lights_"),
            acting: fs(&["lights_set", "lights_match"]), clean: fs(&["lights_status"]),
            say: map(&[("lights_set", "change the lights"), ("lights_match", "change the lights")]),
            code: true, listing: true, native_sheet: true, cli: vec![s("lights")],
            cli_help: s("The owner's lights (Govee, HappyLighting, Sylvania/Tuya): scan, list, name, set. \
                         See docs/lights.md"),
            ..App::default()
        },
        App {
            slot: s("spotify"), module: s("spotify"), make: s("make_spotify"), switch: s("EVE_SPOTIFY"),
            default_on: true,
            title: s("Music"), symbol: s("music.note"), detail: s("Spotify"), doc: s("docs/spotify.md"),
            no_control: s("eve has no Spotify login on this computer."),
            label: s("music"), tool_prefix: s("spotify_"),
            acting: fs(&["spotify_control"]),
            sources: map(&[("spotify_control", "Spotify"), ("spotify_now_playing", "Spotify")]),
            say: map(&[("spotify_control", "change the music")]),
            own_words: [(s("spotify_control"), fs(&["pause", "volume", "transfer", "mode_on", "mode_off", "mode"]))]
                .into_iter()
                .collect(),
            code: true, code_taint_tool: s("spotify_control"), native_sheet: true, cli: vec![s("spotify")],
            cli_help: s("The owner's Spotify (spotKnob's powers): login, status, devices, play, pause, next, \
                         previous, volume, transfer. See docs/spotify.md"),
            ..App::default()
        },
        App {
            slot: s("firetv"), module: s("firetv"), make: s("make_firetv"), switch: s("EVE_FIRETV"), default_on: true,
            title: s("Remote"), symbol: s("tv"), detail: s("Fire TV"), doc: s("docs/firetv.md"),
            no_control: s("eve has no TV on this computer."),
            label: s("tv_control"), tool_prefix: s("firetv_"),
            acting: fs(&["firetv_control"]), clean: fs(&["firetv_status"]),
            say: map(&[("firetv_control", "use the TV")]),
            // no screenshot out loud: the picture goes to a chat, and a voice conversation has none
            voice_omit: fs(&["firetv_screenshot"]),
            voice_kwargs: [(s("screenshot"), Value::Bool(false))].into_iter().collect(),
            voice_note: s("Out loud you can't see the TV's screen: when you need to know what it shows, ask the \
                           owner."),
            code: true, native_sheet: true, cli: vec![s("firetv")],
            cli_help: s("The owner's Fire TV over network ADB: pair, status, on, off, key, volume, mute, open, apps, \
                         youtube, type, screenshot, forget. See docs/firetv.md"),
            ..App::default()
        },
        App {
            slot: s("obsbot"), module: s("obsbot_panel"), make: s("make_panel"), switch: s("EVE_OBSBOT"),
            default_on: true,
            title: s("Camera"), symbol: s("web.camera"), detail: s("OBSBOT webcam"), doc: s("docs/obsbot.md"),
            no_control: s("The camera is off on this Mac (no OBSBOT camera is plugged in, or EVE_OBSBOT=0)."),
            label: s("camera_aim"), tool_prefix: s("camera_"),
            acting: fs(&["camera_control"]), clean: fs(&["camera_status"]),
            say: map(&[("camera_control", "move the camera")]),
            code: true, stream: true, native_sheet: true, cli: vec![s("obsbot"), s("cam")], cli_module: s("obsbot"),
            cli_help: s("The OBSBOT Tiny 3 webcam through OBSBOT Center's OSC: setup, status, wake, sleep, move, \
                         nudge, center, zoom, fov, preset, track, mode. See docs/obsbot.md"),
            ..App::default()
        },
        App {
            slot: s("pika"), module: s("pika"), make: s("make_pika"), switch: s("EVE_PIKA"), default_on: false,
            title: s("Pika"), symbol: s("car"), detail: s("Drive the car"), doc: s("docs/pika.md"),
            no_control: s("Pika isn't set up on this Mac (EVE_PIKA and pika.json)."),
            tools: false, stream: true, native_sheet: true,
            ..App::default()
        },
        // eve-bridge app new: new apps go above this line
    ]
});

/// The code path's order (TelegramInlet._handle): the TV first, so a light named "TV strip" cannot claim "turn on
/// the tv", then the camera (every pattern names it), the lights, the music.
pub const CODE_ORDER: &[&str] = &["firetv", "obsbot", "lights", "spotify"];
/// The phone's Apps list, after Notes (which is eve's own, never a registry app).
pub const PHONE_ORDER: &[&str] = &["spotify", "lights", "firetv", "pika", "obsbot"];
/// Names a new app may not take: eve's own panels and the CLI word.
pub const RESERVED: &[&str] = &["memory", "notes", "app"];
/// The line `eve-bridge app new` writes above, in the Python registry (`app_registry.py`).
pub const MARKER: &str = "# eve-bridge app new: new apps go above this line";

const ON: &[&str] = &["1", "true", "yes", "on"];
const OFF: &[&str] = &["0", "false", "no", "off"];

pub fn get(slot: &str) -> Option<&'static App> {
    APPS.iter().find(|a| a.slot == slot)
}

/// The apps that give the models tools, in tool order.
pub fn with_tools() -> Vec<&'static App> {
    APPS.iter().filter(|a| a.tools).collect()
}

/// The listed slots first, then every other app in APPS order: a new app lands last in every order.
pub fn ordered<I, S>(order: I) -> Vec<&'static App>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut named: Vec<&'static App> = order.into_iter().filter_map(|s| get(s.as_ref())).collect();
    let rest: Vec<&'static App> = APPS.iter().filter(|a| !named.iter().any(|n| n == a)).collect();
    named.extend(rest);
    named
}

pub fn by_code() -> Vec<&'static App> {
    ordered(CODE_ORDER).into_iter().filter(|a| a.code).collect()
}

pub fn for_phone() -> Vec<&'static App> {
    ordered(PHONE_ORDER).into_iter().filter(|a| a.controls).collect()
}

/// The switch alone (a module may also need a login, a file or a device). `environ` None reads the process
/// environment.
pub fn switched_on(app: &App, environ: Option<&HashMap<String, String>>) -> bool {
    let raw = match environ {
        Some(env) => env.get(&app.switch).cloned().unwrap_or_default(),
        None => std::env::var(&app.switch).unwrap_or_default(),
    };
    let raw = raw.trim().to_lowercase();
    if app.default_on {
        !OFF.contains(&raw.as_str())
    } else {
        ON.contains(&raw.as_str())
    }
}

/// The tools app whose TOOL_NAMES holds `name`; out loud (`voice`) its voice_omit names are no one's. Reads the
/// registered modules.
pub fn owner(name: &str, voice: bool) -> Option<&'static App> {
    owner_in(name, voice, &Registered)
}

/// [`owner`] against a given module source. An app whose module is not in it holds no names.
pub fn owner_in(name: &str, voice: bool, src: &dyn ModuleSource) -> Option<&'static App> {
    for app in with_tools() {
        let Some(module) = app.module_in(src) else {
            tracing::warn!("app_registry: module {} is not registered; {} holds no tools", app.module, app.slot);
            continue;
        };
        if module.tool_names().iter().any(|n| n == name) && !(voice && app.voice_omit.contains(name)) {
            return Some(app);
        }
    }
    None
}

/// Python's `_union(field_name)` over a set field.
fn union(field: fn(&App) -> &BTreeSet<String>) -> BTreeSet<String> {
    APPS.iter().flat_map(|a| field(a).iter().cloned()).collect()
}

/// `ACTING()`: named after the tool_gate set it feeds.
pub fn acting() -> BTreeSet<String> {
    union(|a| &a.acting)
}

/// `CLEAN()`.
pub fn clean() -> BTreeSet<String> {
    union(|a| &a.clean)
}

/// `SOURCES()`.
pub fn sources() -> IndexMap<String, String> {
    APPS.iter().flat_map(|a| a.sources.iter().map(|(k, v)| (k.clone(), v.clone()))).collect()
}

/// `SAY()`.
pub fn say() -> IndexMap<String, String> {
    APPS.iter().flat_map(|a| a.say.iter().map(|(k, v)| (k.clone(), v.clone()))).collect()
}

/// `OWN_WORDS()`.
pub fn own_words() -> IndexMap<String, BTreeSet<String>> {
    APPS.iter().flat_map(|a| a.own_words.iter().map(|(k, v)| (k.clone(), v.clone()))).collect()
}

pub fn labels() -> Vec<&'static str> {
    APPS.iter().filter(|a| !a.label.is_empty()).map(|a| a.label.as_str()).collect()
}

pub fn prefixes() -> Vec<(&'static str, &'static str)> {
    APPS.iter()
        .filter(|a| !a.tool_prefix.is_empty() && !a.label.is_empty())
        .map(|a| (a.tool_prefix.as_str(), a.label.as_str()))
        .collect()
}

/// Python's `TypeError` from [`check_hubs`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unexpected app hub(s): {} (not a tools app in app_registry.APPS)", .0.join(", "))]
pub struct UnexpectedHubs(pub Vec<String>);

/// A site's `**app_hubs`: every key must be a tools app's slot.
pub fn check_hubs<I, S>(hubs: I) -> Result<(), UnexpectedHubs>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let known: BTreeSet<&str> = with_tools().into_iter().map(|a| a.slot.as_str()).collect();
    let bad: BTreeSet<String> =
        hubs.into_iter().map(|h| h.as_ref().to_string()).filter(|h| !known.contains(h.as_str())).collect();
    if bad.is_empty() {
        Ok(())
    } else {
        Err(UnexpectedHubs(bad.into_iter().collect()))
    }
}

/// The contract an app must meet; [] when it does. Reads the app's registered module.
pub fn check(app: &App) -> Vec<String> {
    check_in(app, &Registered)
}

/// [`check`] against a given module source.
pub fn check_in(app: &App, src: &dyn ModuleSource) -> Vec<String> {
    let mut problems: Vec<String> = Vec::new();
    if !is_identifier(&app.slot) || !is_lower(&app.slot) || RESERVED.contains(&app.slot.as_str()) {
        problems.push(format!(
            "slot {} is not a lowercase name outside {}",
            py_str_repr(&app.slot),
            py_list_repr(&sorted_reserved())
        ));
    }
    if !app.switch.starts_with("EVE_") {
        problems.push(format!("switch {} does not start with EVE_", py_str_repr(&app.switch)));
    }
    let Some(module) = app.module_in(src) else {
        // Python's import would raise here; the contract cannot be read any further
        problems.push(format!("eve_bridge.{} is not registered", app.module));
        return problems;
    };
    if !module.callable(&app.make) {
        problems.push(format!("{}.{} is missing", app.module, app.make));
    }
    if app.tools {
        let names: Vec<String> = match &module.tool_names {
            Some(n) if !n.is_empty() => n.clone(),
            _ => {
                problems.push(format!("{}.TOOL_NAMES is missing", app.module));
                Vec::new()
            }
        };
        if module.off_reason.is_none() {
            problems.push(format!("{}.OFF_REASON is missing", app.module));
        }
        let tools_ok = match &module.tools {
            Some(Value::Array(tools)) => {
                let tool_names: Vec<Value> =
                    tools.iter().filter_map(|t| t.as_object()).map(|t| t.get("name").cloned().unwrap_or(Value::Null)).collect();
                tool_names.len() == names.len()
                    && tool_names.iter().zip(&names).all(|(t, n)| t.as_str() == Some(n.as_str()))
            }
            _ => false,
        };
        if !tools_ok {
            problems.push(format!("{}.TOOLS is missing, or its names are not TOOL_NAMES in order", app.module));
        }
        // getattr(mod, "INSTRUCTIONS", getattr(mod, "INSTRUCTIONS_HEAD", None)) is a str
        if module.instructions.is_none() && module.instructions_head.is_none() {
            problems.push(format!("{}.INSTRUCTIONS is missing (the intent eval lends it)", app.module));
        }
        let mut mentioned: BTreeSet<&String> = BTreeSet::new();
        mentioned.extend(&app.acting);
        mentioned.extend(&app.clean);
        mentioned.extend(&app.voice_omit);
        mentioned.extend(app.sources.keys());
        mentioned.extend(app.say.keys());
        mentioned.extend(app.own_words.keys());
        for name in mentioned {
            if !names.contains(name) {
                problems.push(format!("{} is not one of {}.TOOL_NAMES", name, app.module));
            }
        }
        if app.tool_prefix.is_empty() {
            problems.push("a tools app needs a tool_prefix".to_string());
        }
        for n in &names {
            if !app.tool_prefix.is_empty() && !n.starts_with(&app.tool_prefix) {
                problems.push(format!("{} does not start with {}", n, app.tool_prefix));
            }
        }
        if !app.code_taint_tool.is_empty() && !names.contains(&app.code_taint_tool) {
            problems.push(format!("code_taint_tool {} is not one of its tools", app.code_taint_tool));
        }
    }
    let hub_cls = hub_class(&module, &app.make);
    let hub_has = |method: &str| hub_cls.as_ref().is_some_and(|c| c.has(method));
    if app.code {
        if !module.callable("match") && !hub_has("match") {
            problems.push("a code app needs a match(text)".to_string());
        }
        if !hub_has("run") {
            problems.push("a code app's hub needs run(cmd)".to_string());
        }
    }
    if app.controls && !hub_has("panel") {
        problems.push("a controls app's hub needs panel(body)".to_string());
    }
    if app.stream && !hub_has("stream") {
        problems.push("a stream app's hub needs stream(writer)".to_string());
    }
    if !app.cli.is_empty() {
        match app.cli_module_in(src) {
            // Python's import of the CLI module would raise here
            None => problems.push(format!("eve_bridge.{} is not registered", app.cli_module_name())),
            Some(m) if !m.callable("cli") => problems.push(format!("{}.cli is missing", app.cli_module_name())),
            Some(_) => {}
        }
    }
    problems
}

/// The class an app's factory returns, from its `Optional[X]` annotation, else the module's one class with `panel`
/// and `close`: enough for [`check`] to see the hub's methods without making one (no device is touched).
pub fn hub_class(module: &ModuleInfo, make: &str) -> Option<ClassInfo> {
    if !make.is_empty() && module.callable(make) {
        if let Some(ret) = module.returns.get(make) {
            return Some(ret.clone());
        }
    }
    let found: Vec<&ClassInfo> = module.classes.iter().filter(|c| c.has("panel") && c.has("close")).collect();
    if found.len() == 1 {
        Some(found[0].clone())
    } else {
        None
    }
}

// ---- the generic phone sheet (docs/adding-an-app.md, "The phone") ---------------------------------------------------

pub const UI_VERSION: i64 = 1;
pub const MAX_CONTROLS: usize = 24;
pub const CONTROL_TYPES: &[&str] = &["button", "toggle", "slider"];

pub fn button(id: &str, label: &str, action: &str, symbol: &str, destructive: bool) -> Value {
    let mut out = Map::new();
    out.insert("type".into(), "button".into());
    out.insert("id".into(), id.into());
    out.insert("label".into(), label.into());
    out.insert("action".into(), action.into());
    if !symbol.is_empty() {
        out.insert("symbol".into(), symbol.into());
    }
    if destructive {
        out.insert("role".into(), "destructive".into());
    }
    Value::Object(out)
}

pub fn toggle(id: &str, label: &str, on: bool, action: &str) -> Value {
    let mut out = Map::new();
    out.insert("type".into(), "toggle".into());
    out.insert("id".into(), id.into());
    out.insert("label".into(), label.into());
    out.insert("on".into(), on.into());
    out.insert("action".into(), action.into());
    Value::Object(out)
}

/// A slider. The numbers keep their JSON kind (an integer stays an integer, as Python's `3` does).
pub fn slider(
    id: &str,
    label: &str,
    value: impl Into<Value>,
    lo: impl Into<Value>,
    hi: impl Into<Value>,
    action: &str,
    step: Option<Value>,
) -> Value {
    let mut out = Map::new();
    out.insert("type".into(), "slider".into());
    out.insert("id".into(), id.into());
    out.insert("label".into(), label.into());
    out.insert("value".into(), value.into());
    out.insert("min".into(), lo.into());
    out.insert("max".into(), hi.into());
    out.insert("action".into(), action.into());
    if let Some(step) = step {
        out.insert("step".into(), step);
    }
    Value::Object(out)
}

/// A generic sheet's answer: `status` is the headline, `line` the muted said-line, `controls` redraw the sheet (None
/// keeps the last ones on the phone).
pub fn panel(status: &str, controls: Option<Vec<Value>>, line: &str, ok: bool) -> Value {
    let mut out = Map::new();
    out.insert("ok".into(), ok.into());
    out.insert("ui".into(), UI_VERSION.into());
    if !line.is_empty() {
        out.insert("line".into(), line.into());
    }
    if !status.is_empty() {
        out.insert("status".into(), status.into());
    }
    if let Some(controls) = controls {
        out.insert("controls".into(), Value::Array(controls));
    }
    Value::Object(out)
}

/// What the phone's generic sheet would refuse or drop in a panel answer; [] when it is all shown.
pub fn check_panel(obj: &Value) -> Vec<String> {
    let Some(obj) = obj.as_object() else {
        return vec!["the answer is not a JSON object".to_string()];
    };
    let mut problems: Vec<String> = Vec::new();
    if !matches!(obj.get("ok"), Some(Value::Bool(_))) {
        problems.push("ok must be true or false".to_string());
    }
    for key in ["line", "status"] {
        if let Some(v) = obj.get(key) {
            if !v.is_string() {
                problems.push(format!("{key} must be a string"));
            }
        }
    }
    if obj.get("ok") == Some(&Value::Bool(false)) {
        return problems;
    }
    if !py_eq_int(obj.get("ui"), UI_VERSION) {
        problems.push(format!("ui must be {UI_VERSION}"));
    }
    let controls = match obj.get("controls") {
        None | Some(Value::Null) => return problems,
        Some(Value::Array(c)) => c,
        Some(_) => {
            problems.push("controls must be a list".to_string());
            return problems;
        }
    };
    if controls.len() > MAX_CONTROLS {
        problems.push(format!("more than {MAX_CONTROLS} controls"));
    }
    let mut ids: BTreeSet<String> = BTreeSet::new();
    for (i, c) in controls.iter().enumerate() {
        let place = format!("controls[{i}]");
        let Some(c) = c.as_object() else {
            problems.push(format!("{place} is not an object"));
            continue;
        };
        let kind = c.get("type").unwrap_or(&Value::Null);
        let kind_str = kind.as_str().filter(|k| CONTROL_TYPES.contains(k));
        let Some(kind) = kind_str else {
            problems.push(format!(
                "{place}: type {} is not one of {}",
                py_repr(kind),
                CONTROL_TYPES.join(", ")
            ));
            continue;
        };
        for key in ["id", "label", "action"] {
            if c.get(key).and_then(Value::as_str).is_none_or(str::is_empty) {
                problems.push(format!("{place}: {key} must be a non-empty string"));
            }
        }
        let id = c.get("id").unwrap_or(&Value::Null);
        if let Some(id_str) = id.as_str() {
            if ids.contains(id_str) {
                problems.push(format!("{place}: id {} is used twice", py_str_repr(id_str)));
            }
        }
        ids.insert(py_str(id));
        if kind == "toggle" && !matches!(c.get("on"), Some(Value::Bool(_))) {
            problems.push(format!("{place}: a toggle needs on"));
        }
        if kind == "slider" {
            let nums: Vec<Option<&Value>> = ["value", "min", "max"].iter().map(|k| c.get(*k)).collect();
            if !nums.iter().all(|n| n.is_some_and(is_number)) {
                problems.push(format!("{place}: a slider needs numeric value, min and max"));
            } else if !num_lt(nums[1].unwrap_or(&Value::Null), nums[2].unwrap_or(&Value::Null)) {
                problems.push(format!("{place}: a slider needs min < max"));
            }
            if let Some(step) = c.get("step") {
                if !(is_number(step) && num_lt(&Value::from(0), step)) {
                    problems.push(format!("{place}: step must be a positive number"));
                }
            }
        }
        if kind == "button" {
            match c.get("role") {
                None | Some(Value::Null) => {}
                Some(Value::String(r)) if r == "destructive" => {}
                Some(_) => problems.push(format!("{place}: role is destructive or absent")),
            }
        }
    }
    problems
}

/// Python's `_number`: an int or float, not a bool, finite.
fn is_number(v: &Value) -> bool {
    match v {
        Value::Number(n) => n.as_f64().is_some_and(f64::is_finite),
        _ => false,
    }
}

/// `a < b` for two JSON numbers, exact for integers as Python's is.
fn num_lt(a: &Value, b: &Value) -> bool {
    let (Value::Number(a), Value::Number(b)) = (a, b) else {
        return false;
    };
    if let (Some(x), Some(y)) = (a.as_i64(), b.as_i64()) {
        return x < y;
    }
    if let (Some(x), Some(y)) = (a.as_u64(), b.as_u64()) {
        return x < y;
    }
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x < y,
        _ => false,
    }
}

/// Python's `v == n` for a JSON value and an int: `1 == 1.0 == True`.
fn py_eq_int(v: Option<&Value>, n: i64) -> bool {
    match v {
        Some(Value::Bool(b)) => i64::from(*b) == n,
        Some(Value::Number(x)) => {
            if let Some(i) = x.as_i64() {
                i == n
            } else if x.is_u64() {
                false
            } else {
                x.as_f64() == Some(n as f64)
            }
        }
        _ => false,
    }
}

// ---- Python's repr and str, for the messages ------------------------------------------------------------------------

/// Python's `repr(str)`.
pub fn py_str_repr(s: &str) -> String {
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
            c if !py_printable(c) => {
                let code = c as u32;
                if code <= 0xff {
                    out.push_str(&format!("\\x{code:02x}"));
                } else if code <= 0xffff {
                    out.push_str(&format!("\\u{code:04x}"));
                } else {
                    out.push_str(&format!("\\U{code:08x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python's `str.isprintable()` for one character, which `repr()` escapes when false: not a control (Cc), format
/// (Cf), private-use (Co), surrogate, noncharacter, or separator other than the space (Zs, Zl, Zp). Unassigned code
/// points (Cn) beyond the noncharacters need Unicode's tables and print as themselves.
fn py_printable(c: char) -> bool {
    let u = c as u32;
    if c == ' ' {
        return true;
    }
    if c.is_control() {
        return false; // Cc
    }
    let separator = matches!(u, 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000);
    let format = matches!(
        u,
        0xad | 0x600..=0x605
            | 0x61c
            | 0x6dd
            | 0x70f
            | 0x890..=0x891
            | 0x8e2
            | 0x180e
            | 0x200b..=0x200f
            | 0x202a..=0x202e
            | 0x2060..=0x2064
            | 0x2066..=0x206f
            | 0xfeff
            | 0xfff9..=0xfffb
            | 0x110bd
            | 0x110cd
            | 0x13430..=0x1343f
            | 0x1bca0..=0x1bca3
            | 0x1d173..=0x1d17a
            | 0xe0001
            | 0xe0020..=0xe007f
    );
    let private = matches!(u, 0xe000..=0xf8ff | 0xf0000..=0xffffd | 0x100000..=0x10fffd);
    let nonchar = matches!(u, 0xfdd0..=0xfdef) || (u & 0xfffe) == 0xfffe;
    !(separator || format || private || nonchar)
}

fn py_list_repr(items: &[&str]) -> String {
    format!("[{}]", items.iter().map(|i| py_str_repr(i)).collect::<Vec<_>>().join(", "))
}

/// Python's `repr(float)`: the shortest round-trip digits, positional for exponents -4 to 15, else scientific.
fn py_float_repr(v: f64) -> String {
    if v.is_nan() {
        return "nan".to_string();
    }
    if v.is_infinite() {
        return if v > 0.0 { "inf".to_string() } else { "-inf".to_string() };
    }
    let sci = format!("{v:e}"); // e.g. "1.5e0", "-2e-5"
    let (mantissa, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if (-4..16).contains(&exp) {
        let plain = format!("{v}");
        if plain.contains('.') {
            plain
        } else {
            format!("{plain}.0")
        }
    } else {
        let sign = if exp < 0 { '-' } else { '+' };
        format!("{mantissa}e{sign}{:02}", exp.abs())
    }
}

/// Python's `repr()` of a JSON value.
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::Null => "None".to_string(),
        Value::Bool(true) => "True".to_string(),
        Value::Bool(false) => "False".to_string(),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else {
                py_float_repr(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        Value::String(s) => py_str_repr(s),
        Value::Array(items) => format!("[{}]", items.iter().map(py_repr).collect::<Vec<_>>().join(", ")),
        Value::Object(m) => format!(
            "{{{}}}",
            m.iter().map(|(k, v)| format!("{}: {}", py_str_repr(k), py_repr(v))).collect::<Vec<_>>().join(", ")
        ),
    }
}

/// Python's `str()` of a JSON value: a string is itself, anything else its repr.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => py_repr(other),
    }
}

fn sorted_reserved() -> Vec<&'static str> {
    let mut r = RESERVED.to_vec();
    r.sort_unstable();
    r
}

/// Python's `str.isidentifier()`: a letter or `_`, then letters, digits or `_`.
fn is_identifier(s: &str) -> bool {
    let mut chars = s.chars();
    match chars.next() {
        Some(c) if c == '_' || c.is_alphabetic() => chars.all(|c| c == '_' || c.is_alphanumeric()),
        _ => false,
    }
}

/// Python's `str.islower()`: at least one cased character, and none upper case.
fn is_lower(s: &str) -> bool {
    s.chars().any(char::is_lowercase) && !s.chars().any(char::is_uppercase)
}

// ---- names a new app may take (app_template.scaffold) ---------------------------------------------------------------

/// Where a hub is kept under its slot's name: the chat inlet and the daemon as `_<slot>`, the voice session and the
/// Mini App server as `<slot>`. A new slot must not be a name these already use for something else.
pub const SITES: &[&str] = &["telegram.py", "voice_agent.py", "miniapp.py", "daemon.py", "link_chat.py"];

/// Python's `keyword.kwlist` (3.13).
const PY_KEYWORDS: &[&str] = &[
    "False", "None", "True", "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
    "elif", "else", "except", "finally", "for", "from", "global", "if", "import", "in", "is", "lambda", "nonlocal",
    "not", "or", "pass", "raise", "return", "try", "while", "with", "yield",
];
/// Python's `keyword.softkwlist` (3.13).
const PY_SOFT_KEYWORDS: &[&str] = &["_", "case", "match", "type"];

/// The Python sources the wiring sites live in (`bridge/src/eve_bridge`, beside `app_registry.py`).
pub fn site_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../bridge/src/eve_bridge"))
}

static ATTR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(?:self|server|cls|inlet|session|daemon)\.([A-Za-z_][A-Za-z0-9_]*)").expect("valid attr regex")
});
static DEF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?m)^\s*(?:async\s+)?def\s+([A-Za-z_][A-Za-z0-9_]*)").expect("valid def regex"));
static FIELD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?m)^    ([A-Za-z_][A-Za-z0-9_]*)\s*(?::[^=\n]*)?=").expect("valid field regex")
});
static NAME_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z][a-z0-9]{1,15}$").expect("valid name regex"));

/// Every attribute and member name the wiring sites already use (`self.x`, `server.x`, `def x`, a class field): read
/// from their source, so nothing is imported and no device is touched.
pub fn site_names() -> BTreeSet<String> {
    site_names_in(&site_dir())
}

/// [`site_names`] read from the sites in `dir`.
pub fn site_names_in(dir: &Path) -> BTreeSet<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    for name in SITES {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!("app_registry: cannot read {}: {e}", path.display());
                continue;
            }
        };
        for re in [&*ATTR_RE, &*DEF_RE, &*FIELD_RE] {
            out.extend(re.captures_iter(&text).filter_map(|c| c.get(1)).map(|m| m.as_str().to_string()));
        }
    }
    out
}

/// Why `name` cannot be a new app's slot; "" when it can. `own_prefixes`: eve's own tools' first words; `labels`:
/// names already in use as an intent-eval label or one of eve's own tools (the scaffold reads them from
/// bridge/tools/intent_eval.py and the tool gate). Reads the registered modules and the wiring sites' sources.
pub fn name_problem<P, L>(name: &str, own_prefixes: P, labels: L) -> String
where
    P: IntoIterator,
    P::Item: AsRef<str>,
    L: IntoIterator,
    L::Item: AsRef<str>,
{
    name_problem_in(name, own_prefixes, labels, &Registered, &site_dir())
}

/// [`name_problem`] against a given module source and wiring-site directory.
pub fn name_problem_in<P, L>(name: &str, own_prefixes: P, labels: L, src: &dyn ModuleSource, sites: &Path) -> String
where
    P: IntoIterator,
    P::Item: AsRef<str>,
    L: IntoIterator,
    L::Item: AsRef<str>,
{
    if !NAME_RE.is_match(name) {
        return "a name is 2 to 16 lowercase letters or digits, starting with a letter".to_string();
    }
    if PY_KEYWORDS.contains(&name) || PY_SOFT_KEYWORDS.contains(&name) {
        return format!("{name} is a Python keyword");
    }
    if RESERVED.contains(&name) || get(name).is_some() {
        return format!("{name} is taken");
    }
    let prefix = format!("{name}_");
    for own in own_prefixes {
        let own = own.as_ref();
        if own.starts_with(&prefix) || prefix.starts_with(own) {
            return format!("{name}_ would collide with eve's own tools ({own}…)");
        }
    }
    for app in with_tools() {
        let mine = &app.tool_prefix;
        if !mine.is_empty() && (mine.starts_with(&prefix) || prefix.starts_with(mine.as_str())) {
            return format!("{name}_ would collide with the {} app's tools ({mine}…)", app.slot);
        }
        let Some(module) = app.module_in(src) else {
            tracing::warn!("app_registry: module {} is not registered; its tool names are not checked", app.module);
            continue;
        };
        if let Some(clash) = module.tool_names().iter().find(|n| n.as_str() == name || n.starts_with(&prefix)) {
            return format!("{name}_ would collide with the {} app's tool {clash}", app.slot);
        }
    }
    let taken = labels.into_iter().any(|l| l.as_ref() == name) || APPS.iter().any(|a| a.label == name);
    if taken {
        return format!("{name} is already an intent-eval label or one of eve's tools");
    }
    let used = site_names_in(sites);
    for attr in [name.to_string(), format!("_{name}")] {
        if used.contains(&attr) {
            return format!("{name} would clash with {attr}, a name eve's wiring already uses");
        }
    }
    String::new()
}

#[cfg(test)]
mod tests;
