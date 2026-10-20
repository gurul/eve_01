//! eve's two editions, full and lite, and the guard eve lite's daemon runs inside (port of `profile.py`).
//!
//! eve lite (docs/plans/eve-lite.md, docs/lite.md) is a second way to install and run eve, for people other than the
//! owner: one pasted line, two browser sign-ins (OpenRouter, Composio), and a web app on the same Wi-Fi. It is the
//! same code with other switches, chosen by one variable:
//!
//! ```text
//!     EVE_PROFILE    unset or "full": today's eve, the owner's, unchanged
//!                    "lite": eve lite
//!                    anything else: ProfileError, and the CLI exits 78 (EX_CONFIG)
//! ```
//!
//! Only the lite installer writes it (the env file, and the LaunchAgent's own environment, which the env file cannot
//! override: envfile fills gaps only). Every lite branch in the code asks [`is_lite`]. With EVE_PROFILE unset nothing
//! here changes anything: [`apply`] returns [] and leaves the environment as it was, [`install_firewall`] installs
//! nothing.
//!
//! What eve lite is made of here:
//!
//! 1. [`apply`], run by the CLI right after the env file loads. It forces every switch in [`LITE_FORCED`], so a door,
//!    lane, device or service eve lite does not have is off however the environment was set; it fills
//!    [`LITE_DEFAULTS`] where they are unset (lite's own IPC socket, never full eve's /tmp one); and it removes every
//!    secret-shaped variable but [`LITE_SECRET_ALLOW`], so a key left in a shell can never switch a feature on. One
//!    log line, names only.
//! 2. [`LITE_CLASSIFIED`]: every EVE_* name a module of the lite composition reads, and why lite forces it or leaves
//!    it alone. The classified test works the module set out from the imports and fails on a name missing here, so a
//!    new switch in code eve lite runs is a decision someone made, not an accident.
//! 3. [`LITE_TOOL_NAMES`] and [`LITE_TOOL_PREFIXES`]: the only tools the lite brain offers and runs (telegram filters
//!    with [`filter_tools`] and refuses with [`lite_tool_allowed`] before any handler).
//! 4. [`install_firewall`], the first thing the lite daemon does: a process firewall that cannot be removed once
//!    added (see [`firewall`]). A process may start only when its program is one of [`EXEC_ALLOW`]; a file inside
//!    the folders macOS guards with a privacy prompt ([`PROTECTED_HOME`], [`PROTECTED_ROOTS`]) is refused, as is a
//!    path that reaches one through symlinks. A refusal is a PermissionError where the call was made. The guard logs
//!    each program or folder name once, never a path, an argument or a value. Mode "audit" refuses nothing and only
//!    counts and logs.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::LazyLock;

use indexmap::IndexMap;
use serde_json::Value;

pub mod firewall;
pub mod pypath;

pub use firewall::{
    audit, audit_command, audit_path, code_roots, firewall_counts, install_firewall, output, popen_event,
    protected_roots, spawn, status, Arg, Event, FileEvent, FirewallError, Guard, Mode, Refusal, Refused, EXEC_ALLOW,
    FIREWALL_MODES, PROTECTED_HOME, PROTECTED_ROOTS,
};

pub const PROFILE_ENV: &str = "EVE_PROFILE";
pub const FULL: &str = "full";
pub const LITE: &str = "lite";
/// The lite brain's one chat: the web app's (telegram's configured() gives it as the owner).
pub const LITE_CHAT_ID: i64 = 1;

/// EVE_PROFILE holds something other than full or lite. The CLI exits 78 (EX_CONFIG) on it. Python's
/// `ProfileError(ValueError)`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ProfileError(pub String);

// ---- environments ---------------------------------------------------------------------------------------------------

/// A mutable environment mapping, as `apply` takes one (Python's `MutableMapping[str, str]`): a map of names to
/// values, or the process environment itself ([`ProcessEnv`]).
pub trait EnvMap {
    fn env_get(&self, name: &str) -> Option<String>;
    /// Every name, in the mapping's own order.
    fn env_names(&self) -> Vec<String>;
    fn env_set(&mut self, name: &str, value: &str);
    fn env_remove(&mut self, name: &str);
}

impl EnvMap for HashMap<String, String> {
    fn env_get(&self, name: &str) -> Option<String> {
        self.get(name).cloned()
    }
    fn env_names(&self) -> Vec<String> {
        self.keys().cloned().collect()
    }
    fn env_set(&mut self, name: &str, value: &str) {
        self.insert(name.to_string(), value.to_string());
    }
    fn env_remove(&mut self, name: &str) {
        self.remove(name);
    }
}

impl EnvMap for IndexMap<String, String> {
    fn env_get(&self, name: &str) -> Option<String> {
        self.get(name).cloned()
    }
    fn env_names(&self) -> Vec<String> {
        self.keys().cloned().collect()
    }
    fn env_set(&mut self, name: &str, value: &str) {
        self.insert(name.to_string(), value.to_string());
    }
    fn env_remove(&mut self, name: &str) {
        // `del environ[name]` on a dict: the others keep their order
        self.shift_remove(name);
    }
}

impl EnvMap for BTreeMap<String, String> {
    fn env_get(&self, name: &str) -> Option<String> {
        self.get(name).cloned()
    }
    fn env_names(&self) -> Vec<String> {
        self.keys().cloned().collect()
    }
    fn env_set(&mut self, name: &str, value: &str) {
        self.insert(name.to_string(), value.to_string());
    }
    fn env_remove(&mut self, name: &str) {
        self.remove(name);
    }
}

/// This process's environment (Python's `os.environ`). Writes go to the process environment, so only the CLI's
/// start, before any thread reads it, should pass this to [`apply`].
#[derive(Debug, Default, Clone, Copy)]
pub struct ProcessEnv;

impl EnvMap for ProcessEnv {
    fn env_get(&self, name: &str) -> Option<String> {
        std::env::var_os(name).map(|v| v.to_string_lossy().into_owned())
    }
    fn env_names(&self) -> Vec<String> {
        std::env::vars_os().map(|(k, _)| k.to_string_lossy().into_owned()).collect()
    }
    fn env_set(&mut self, name: &str, value: &str) {
        std::env::set_var(name, value);
    }
    fn env_remove(&mut self, name: &str) {
        std::env::remove_var(name);
    }
}

/// Python's `str.strip()` with no argument: the characters `str.isspace()` calls whitespace.
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// The edition for `env` (default: this process's environment): "full" or "lite". ProfileError otherwise.
pub fn active(env: Option<&HashMap<String, String>>) -> Result<&'static str, ProfileError> {
    match env {
        Some(env) => active_in(env),
        None => active_in(&ProcessEnv),
    }
}

/// [`active`] for any environment mapping.
pub fn active_in<E: EnvMap + ?Sized>(env: &E) -> Result<&'static str, ProfileError> {
    let raw = env.env_get(PROFILE_ENV);
    let value = py_strip(raw.as_deref().unwrap_or("")).to_lowercase();
    if value.is_empty() || value == FULL {
        return Ok(FULL);
    }
    if value == LITE {
        return Ok(LITE);
    }
    let shown: String = raw.as_deref().unwrap_or("None").chars().take(24).collect();
    Err(ProfileError(format!(
        "{PROFILE_ENV} must be {} or {}, or unset; not {}",
        eve_app_registry::py_str_repr(LITE),
        eve_app_registry::py_str_repr(FULL),
        eve_app_registry::py_str_repr(&shown)
    )))
}

/// Whether `env` (default: this process's environment) is eve lite. Every lite branch asks here.
pub fn is_lite(env: Option<&HashMap<String, String>>) -> Result<bool, ProfileError> {
    Ok(active(env)? == LITE)
}

/// [`is_lite`] for any environment mapping.
pub fn is_lite_in<E: EnvMap + ?Sized>(env: &E) -> Result<bool, ProfileError> {
    Ok(active_in(env)? == LITE)
}

// ---- the switches eve lite forces (docs/plans/eve-lite.md M1) -------------------------------------------------------
//
// Each value reads as off (or as lite's one choice) through its own module's configured() or enabled()
// (test_profile_lite_gate.py checks every one, against an environment that turned it on). Groups as in the spec.

const LITE_FORCED_BASE: &[(&str, &str)] = &[
    // doors: the brain is app-only and nothing reaches Telegram, the Mini App, the ntfy rendezvous or push
    ("EVE_TELEGRAM", "app"), ("EVE_MINIAPP", "0"), ("EVE_LINK_RENDEZVOUS", "0"), ("EVE_NOTIFY", "0"),
    // routing: the routers were fitted to full eve's split; Jev, for anything left that calls it, on OpenRouter
    ("EVE_JEV_ROUTE", "openrouter"), ("EVE_TOOLSET_ROUTER", "off"), ("EVE_GRADE_ROUTER", "off"),
    ("EVE_SEARCH_ROUTER", "off"), ("EVE_MENU_TOOLS", "off"),
    // memory: on, with OpenRouter's embedder; no second-brain vault, claude-mem or ROS bridge
    ("EVE_MEMORY", "1"), ("EVE_MEM0_EMBEDDER", "openrouter"), ("EVE_SECOND_BRAIN", "0"), ("EVE_CLAUDE_MEM", "0"),
    ("EVE_ROSBRIDGE", "0"),
    // apps: Composio Connect only (composio_connect); not direct Google, not Composio's Platform path, no sharing.
    // Gmail drafts run as routine writes; a send still asks with the whole email (composio_tools.decide)
    ("EVE_APPS", "0"), ("EVE_COMPOSIO", "0"), ("EVE_COMPOSIO_POLICY", "gmail=auto"), ("EVE_SHARE", "0"),
    // web: the plain reader and OpenRouter only (no TinyFish, Firecrawl, Monid, headless Chromium or curl_cffi)
    ("EVE_WEB_READER", "0"), ("EVE_MONID", "0"), ("EVE_WATCH_HOSTED", "0"), ("EVE_WATCH_BROWSER", "0"),
    ("EVE_WATCH_TLS", "0"),
    // the Mac: no microphone, keyboard, camera, screen, Codex, lanes, Chrome, coding sessions or learning server
    ("EVE_VOICE", "0"), ("EVE_CALL_LIVE_STT", "0"), ("EVE_LISTEN_KEY", "off"), ("EVE_MAC_CAMERA", "off"),
    ("EVE_SCENE", "0"),
    ("EVE_EXPLORE", "0"), ("EVE_ROBOT", "0"), ("EVE_COMPUTER_CONTROL", "0"), ("EVE_CODEX_PREWARM", "0"),
    ("EVE_DESK_LANE", "0"),
    ("EVE_BROWSER_ATTACH", "0"), ("EVE_BROWSER_OWN", "0"), ("EVE_BROWSER_STEEL", "0"), ("EVE_WORK_SESSIONS", "0"),
    ("EVE_LEARNING", "0"),
    // devices: the owner's apps (app_registry; any app added later is forced off below) and the phone's faces
    ("EVE_LIGHTS", "0"), ("EVE_SPOTIFY", "0"), ("EVE_FIRETV", "0"), ("EVE_PIKA", "0"), ("EVE_OBSBOT", "0"),
    ("EVE_PHONE_EXPRESSIONS", "0"),
    // other: Meet, America, OpenTable, AgentCard, Canvas, the chief of staff, Updates, phone location, the
    // command-risk gate on Claude Code's hooks, and the release check (lite updates by re-running its install line)
    ("EVE_MEET", "0"), ("EVE_AMERICA", "0"), ("EVE_OPENTABLE", "0"), ("EVE_AGENTCARD", "0"), ("EVE_CANVAS", "0"),
    ("EVE_CHIEF", "off"), ("EVE_UPDATES", "0"), ("EVE_PHONE_LOCATION", "0"), ("EVE_COMMAND_RISK", "off"),
    // The New York Times (nyt): its key is secret-shaped and removed anyway; off, its keyless headlines go too
    ("EVE_NYT", "0"),
    ("EVE_BRIDGE_NO_UPDATE_CHECK", "1"),
];

/// Every switch eve lite forces, and its value, in the Python dict's order.
pub static LITE_FORCED: LazyLock<IndexMap<String, String>> = LazyLock::new(|| {
    let mut forced: IndexMap<String, String> =
        LITE_FORCED_BASE.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    // An app added with `eve-bridge app new` brings its own switch (app_registry.APPS): forced off too, so a new
    // app never reaches eve lite by default and its author edits nothing here.
    for app in eve_app_registry::APPS.iter() {
        if !forced.contains_key(&app.switch) {
            forced.insert(app.switch.clone(), "0".to_string());
        }
    }
    forced
});

/// Set only where unset (or empty): lite's IPC socket in its own folder, so it never shares full eve's
/// /tmp/eve-bridge.sock. A leading "~/" is the environment's HOME.
pub static LITE_DEFAULTS: LazyLock<IndexMap<String, String>> =
    LazyLock::new(|| IndexMap::from([("EVE_BRIDGE_SOCK".to_string(), "~/.eve-lite/ipc.sock".to_string())]));

/// The one secret eve lite keeps: the key its OpenRouter sign-in minted. Composio Connect's tokens live in a file
/// (composio-connect.json), not the environment.
pub const LITE_SECRET_ALLOW: &[&str] = &["OPENROUTER_API_KEY"];
pub const SECRET_SUFFIXES: &[&str] = &["_KEY", "_TOKEN", "_SECRET", "_PASSWORD", "_CLIENT_ID"];
/// Not a secret, but it is what opens Telegram.
pub const SECRET_NAMES: &[&str] = &["EVE_TELEGRAM_OWNER"];

/// Names a lite module reads that mention a key, token, secret, password or sign-in and hold none: what each holds.
/// The classified test fails on any other such name until it is shaped like a secret, listed in SECRET_NAMES
/// (apply removes it) or listed here.
pub static NOT_SECRET_NAMES: LazyLock<IndexMap<&'static str, &'static str>> = LazyLock::new(|| {
    IndexMap::from([
        ("EVE_KEY_METHOD", "how the desk's key presses are posted (full eve's desk)"),
        ("EVE_KEY_SOURCE", "which event source the desk's key presses come from (full eve's desk)"),
        ("EVE_CODEX_AUTH", "which sign-in Codex runs with: a mode, not a credential (Codex is off in lite)"),
        ("EVE_VAULT_KEYCHAIN", "where the password vault's keychain file is: a path (the vault is off in lite)"),
    ])
});

/// Whether an environment variable's name looks like a secret's: ends in _KEY, _TOKEN, _SECRET, _PASSWORD or
/// _CLIENT_ID (in any case), or is EVE_TELEGRAM_OWNER.
pub fn is_secret_name(name: &str) -> bool {
    let upper = name.to_uppercase();
    SECRET_SUFFIXES.iter().any(|s| upper.ends_with(s)) || SECRET_NAMES.contains(&upper.as_str())
}

fn expand<E: EnvMap + ?Sized>(value: &str, environ: &E) -> String {
    if let Some(rest) = value.strip_prefix("~/") {
        let home = environ
            .env_get("HOME")
            .filter(|h| !h.is_empty())
            .unwrap_or_else(expanduser_home);
        return pypath::join(&home, rest);
    }
    value.to_string()
}

/// `os.path.expanduser("~")`: this process's HOME, else the account's home folder, either with its trailing "/"
/// taken off ("/" when nothing is left); "~" itself when the account has no entry.
fn expanduser_home() -> String {
    let home = match std::env::var_os("HOME") {
        Some(h) => h.to_string_lossy().into_owned(),
        None => {
            let home = firewall::account_home();
            if home.is_empty() {
                return "~".to_string();
            }
            home
        }
    };
    let trimmed = home.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Make `environ` eve lite's when EVE_PROFILE says lite: the names it changed, sorted. [] and nothing touched
/// otherwise. ProfileError on a value that is neither (the CLI exits 78).
///
/// Secret-shaped variables go first (all but LITE_SECRET_ALLOW, and never a forced or default name: EVE_LISTEN_KEY
/// ends in _KEY and is the keyboard key that opens the Mac's microphone), then every LITE_FORCED value is set, then
/// LITE_DEFAULTS fill what is unset. One log line: counts and names, never a value. The CLI may run this before
/// logging is set up: the names returned let it say them once it is.
pub fn apply<E: EnvMap + ?Sized>(environ: &mut E) -> Result<Vec<String>, ProfileError> {
    if active_in(environ)? != LITE {
        return Ok(Vec::new());
    }
    let keep = |name: &str| {
        LITE_SECRET_ALLOW.contains(&name) || LITE_FORCED.contains_key(name) || LITE_DEFAULTS.contains_key(name)
    };
    let mut removed: Vec<String> =
        environ.env_names().into_iter().filter(|name| is_secret_name(name) && !keep(name)).collect();
    removed.sort();
    removed.dedup();
    for name in &removed {
        environ.env_remove(name);
    }
    let forced: Vec<String> = LITE_FORCED
        .iter()
        .filter(|(name, value)| environ.env_get(name).as_deref() != Some(value.as_str()))
        .map(|(name, _)| name.clone())
        .collect();
    for name in &forced {
        environ.env_set(name, &LITE_FORCED[name.as_str()]);
    }
    let defaulted: Vec<String> = LITE_DEFAULTS
        .keys()
        .filter(|name| py_strip(environ.env_get(name).as_deref().unwrap_or("")).is_empty())
        .cloned()
        .collect();
    for name in &defaulted {
        let value = expand(&LITE_DEFAULTS[name.as_str()], environ);
        environ.env_set(name, &value);
    }
    let nothing = |names: &[String]| if names.is_empty() { "nothing".to_string() } else { names.join(", ") };
    tracing::info!(
        "profile: eve lite: {} switches forced, {} already so; set {}; removed {}",
        forced.len(),
        LITE_FORCED.len() - forced.len(),
        nothing(&defaulted),
        nothing(&removed)
    );
    let changed: BTreeSet<String> = removed.into_iter().chain(forced).chain(defaulted).collect();
    Ok(changed.into_iter().collect())
}

/// [`apply`] on this process's own environment (Python's `apply(os.environ)`).
pub fn apply_process_env() -> Result<Vec<String>, ProfileError> {
    apply(&mut ProcessEnv)
}

// ---- every switch the lite composition reads, and why lite leaves it as it is -----------------------------------
//
// The classified test finds every EVE_* name in the modules the lite daemon imports (its composition and everything
// their imports reach, function-level imports included) and fails on a name missing here. A reason is:
//   "forced to …" / "default when unset: …"     generated from LITE_FORCED and LITE_DEFAULTS below;
//   "secret-shaped: …"                          apply() removes it (is_secret_name);
//   "inert while EVE_X … forced …: …"           a setting of a feature whose switch lite forces (the test checks
//                                               that every EVE_X named there is in LITE_FORCED);
//   "kept: …"                                   a setting of a feature lite runs, whose default is right for lite;
//   "not run in lite: …"                        read only by code lite imports and never runs.
// A name built at run time is written with * for the part that varies (sound.py's EVE_<SOUND>_FILE).

/// `dict.fromkeys(names, reason)`.
fn why(reason: &str, names: &[&str]) -> Vec<(String, String)> {
    names.iter().map(|n| (n.to_string(), reason.to_string())).collect()
}

const MAC_TASKS: &str = "inert while EVE_COMPUTER_CONTROL, EVE_DESK_LANE, EVE_BROWSER_ATTACH and EVE_BROWSER_OWN \
are forced off: a setting of the Mac's tasks (Codex, the desk and browser lanes, launches, the password vault's \
fills); lite's brain has no agent and none of their tools";
const DESK_VOICE: &str = "inert while EVE_VOICE is forced off: a setting of the desk voice and room notes, which use \
the Mac's own microphone; lite talks only through the web app's push-to-talk";
const FULL_DAEMON: &str = "not run in lite: full eve's daemon (daemon.py) and the robot, camera and serial link only \
it builds; lite's daemon (lite.py) never imports it";

/// Every EVE_* name a module of the lite composition reads, and why lite forces it or leaves it alone, in the Python
/// dict's order.
pub static LITE_CLASSIFIED: LazyLock<IndexMap<String, String>> = LazyLock::new(|| {
    let mut entries: Vec<(String, String)> = vec![(
        PROFILE_ENV.to_string(),
        "the edition switch itself: unset or full is the owner's eve, lite is eve lite".to_string(),
    )];
    entries.extend(LITE_FORCED.iter().map(|(n, v)| (n.clone(), format!("forced to {v}"))));
    entries.extend(LITE_DEFAULTS.iter().map(|(n, v)| (n.clone(), format!("default when unset: {v}"))));
    let groups: Vec<(&str, &[&str])> = vec![
        ("secret-shaped: apply() removes it, so no key or owner id switches a feature on",
         &["EVE_TELEGRAM_TOKEN", "EVE_TELEGRAM_OWNER", "EVE_GITHUB_MCP_TOKEN", "EVE_GOOGLE_MAPS_KEY",
           "EVE_OPENAI_ADMIN_KEY", "EVE_ANTHROPIC_ADMIN_KEY", "EVE_ENTER_KEY"]),
        // what eve lite runs
        ("kept: the chat brain's models and efforts (luna; App Think on sol); the defaults are lite's",
         &["EVE_TELEGRAM_MODEL", "EVE_TELEGRAM_EFFORT", "EVE_APP_THINK_MODEL", "EVE_APP_THINK_EFFORT"]),
        ("kept: think_hard (astra), on in lite",
         &["EVE_THINK", "EVE_THINK_MODEL", "EVE_THINK_EFFORT", "EVE_THINK_TIMEOUT_SECS"]),
        ("kept: small talk, on in lite", &["EVE_SMALL_TALK", "EVE_SMALL_TALK_MODEL"]),
        ("kept: science search, on in lite (no key)", &["EVE_SCIENCE_SEARCH"]),
        ("kept: Muse on the owner's Raspberry Pi (muse_pi.py); off until set, in lite and full eve alike",
         &["EVE_MUSE_PI"]),
        ("kept: card pictures from Wikipedia (find_image), on in lite (no key)", &["EVE_FIND_IMAGE"]),
        ("kept: Horizontal History's topic titles, through OpenRouter", &["EVE_TOPIC_TITLES", "EVE_TOPIC_TITLE_MODEL"]),
        ("inert while EVE_BROWSER_STEEL is forced off: a setting of the local Steel browser lane (steel_local.py)",
         &["EVE_STEEL_DIR", "EVE_STEEL_PROFILE", "EVE_STEEL_URL"]),
        ("kept: web search through OpenRouter (Perplexity), on in lite",
         &["EVE_WEB_SEARCH", "EVE_WEB_SEARCH_MODEL", "EVE_WEB_SEARCH_RESULTS", "EVE_WEB_SEARCH_MAX_USES"]),
        ("kept: watches, on in lite, with the plain reader and OpenRouter",
         &["EVE_WATCH", "EVE_WATCH_FILE", "EVE_WATCH_RATE", "EVE_WATCH_BURST", "EVE_WATCH_HOST_GAP",
           "EVE_WATCH_MODEL", "EVE_WATCH_MODEL_CALLS", "EVE_WATCH_VISION_MODEL"]),
        ("kept: the chat log, on in lite",
         &["EVE_CHAT_LOG", "EVE_CHAT_LOG_FILE", "EVE_CHAT_LOG_DAYS", "EVE_CHAT_LOG_LINES"]),
        ("kept: memory, on in lite: its folders (under ~/.config/eve-bridge by default) and its experiments \
(off by default)",
         &["EVE_MEMORY_DIR", "EVE_RECORDS", "EVE_MEM0", "EVE_MEMORY_HYBRID", "EVE_MEMORY_RETRY",
           "EVE_MEMORY_TIMEARGS", "EVE_MEMORY_SOURCES", "EVE_MEMORY_VALIDITY", "EVE_DEBRIEF_DIR", "EVE_NOTES_DIR"]),
        ("kept: push-to-talk in the web app (speech to text and eve's voice, both through OpenRouter)",
         &["EVE_CALL_STT_MODEL", "EVE_CALL_LANGUAGE", "EVE_CALL_OR_TTS_MODEL", "EVE_CALL_OR_VOICE"]),
        ("kept: Jev's model and wait, for anything left that calls it (its route is forced to OpenRouter)",
         &["EVE_JEV_MODEL", "EVE_JEV_TIMEOUT"]),
        ("kept: where spend lines go", &["EVE_SPEND_DIR"]),
        ("kept: the clock's time zone and the weather's place, set by the user or not at all",
         &["EVE_TIMEZONE", "EVE_LOCATION", "EVE_WEATHER_PLACE", "EVE_WEATHER_LAT", "EVE_WEATHER_LON"]),
        ("kept: where the env file is (lite's installer writes the default one)", &["EVE_ENV_FILE"]),
        ("kept: where lite_web serves the web page from (default: the packaged web_lite)", &["EVE_WEB_DIR"]),
        // settings of features lite forces off
        (MAC_TASKS,
         &["EVE_AGENT_EXEC_TIMEOUT", "EVE_AGENT_MAX_SECS", "EVE_AGENT_MAX_TURNS", "EVE_AGENT_MODEL",
           "EVE_AGENT_PLAN_REASONING", "EVE_AGENT_REASONING", "EVE_AGENT_RUNS_DIR", "EVE_AGENT_VERIFY",
           "EVE_AGENT_VERIFY_REASONING", "EVE_BROWSER_INTENT", "EVE_BROWSER_INTENT_IDLE_S", "EVE_BROWSER_MODEL",
           "EVE_FAST_LANE", "EVE_FAST_LANE_DECIDE",
           "EVE_LANE_FIRST", "EVE_PLAN_EXEC", "EVE_PLAN_EXEC_REASONING", "EVE_REFLEXES", "EVE_ROUTER_MODEL",
           "EVE_QUIT_ALL_KEEP", "EVE_CODEX_AUTH", "EVE_CODEX_BIN", "EVE_CODEX_ROUTINE_ACCESS",
           "EVE_CODEX_SITE_ACCESS", "EVE_AX_PRESS", "EVE_DECISIONS", "EVE_JEV_GROUPS", "EVE_JEV_STEP",
           "EVE_CHROME_DEBUG_PORT", "EVE_CHROME_DIR", "EVE_CHROME_PROFILE", "EVE_OWN_CHROME_DIR", "EVE_VAULT_INDEX",
           "EVE_VAULT_KEYCHAIN"]),
        (DESK_VOICE,
         &["EVE_CAPTION_CPS", "EVE_LIVE_BACKEND_EFFORT", "EVE_LIVE_BACKEND_MODEL", "EVE_LIVE_MODEL",
           "EVE_LIVE_WEB_SEARCH", "EVE_VOICE_IDLE_SECS", "EVE_VOICE_NAME", "EVE_VOICE_OUTPUT", "EVE_INTENT",
           "EVE_INTENT_MODEL", "EVE_VOICE_GATE", "EVE_VOICE_GATE_MODEL", "EVE_KWS_MODEL_DIR", "EVE_LESSON_THRESHOLD",
           "EVE_LESSON_WORD", "EVE_MIC", "EVE_MIC_ALWAYS", "EVE_WAKE_THRESHOLD", "EVE_WAKE_WORD",
           "EVE_NOTES_DIARIZE", "EVE_NOTES_KEEP_TRANSCRIPT", "EVE_NOTES_MAX_MINUTES", "EVE_NOTES_MODEL_STT",
           "EVE_NOTES_OWNER_NAME", "EVE_NOTES_OWNER_VOICE", "EVE_NOTES_SEGMENT_SECS"]),
        ("inert while EVE_MINIAPP is forced off: the Mini App, its tunnel and its app builder",
         &["EVE_CLOUDFLARED", "EVE_MINIAPP_DAILY_USD", "EVE_MINIAPP_MAKE_EFFORT", "EVE_MINIAPP_MODEL",
           "EVE_APP_JOURNEYS"]),
        ("inert while EVE_TELEGRAM is forced to app: Telegram's typing drafts; nothing reaches Telegram",
         &["EVE_TELEGRAM_DRAFTS"]),
        ("inert while EVE_LINK_RENDEZVOUS is forced off: the ntfy rendezvous server", &["EVE_LINK_RENDEZVOUS_URL"]),
        ("inert while EVE_NOTIFY is forced off: ntfy push", &["EVE_NOTIFY_PREVIEW", "EVE_NOTIFY_URL"]),
        ("inert while EVE_APPS is forced off: direct Google, GitHub and Maps",
         &["EVE_APPS_DIR", "EVE_APPS_POLICY", "EVE_APPS_TIMEOUT_SECS"]),
        ("inert while EVE_COMPOSIO is forced off: Composio's Platform path (lite's apps go through Composio \
Connect)",
         &["EVE_COMPOSIO_STATE", "EVE_COMPOSIO_TIMEOUT_SECS", "EVE_COMPOSIO_TOOLKITS"]),
        ("inert while EVE_CLAUDE_MEM is forced off: claude-mem", &["EVE_CLAUDE_MEM_MIRROR", "EVE_CLAUDE_MEM_URL"]),
        ("inert while EVE_ROSBRIDGE is forced off: the ROS bridge", &["EVE_ROSBRIDGE_HOST", "EVE_ROSBRIDGE_PORT"]),
        ("inert while EVE_SECOND_BRAIN is forced off: the second-brain vault", &["EVE_VAULT"]),
        ("inert while EVE_MONID is forced off: Monid", &["EVE_MONID_CLI", "EVE_MONID_PER_DAY", "EVE_MONID_USD_PER_DAY"]),
        ("inert while EVE_WEB_READER is forced off: the hosted page reader", &["EVE_WEB_READER_TASKS"]),
        ("inert while EVE_WATCH_HOSTED is forced off: the watcher's hosted fetch", &["EVE_WATCH_HOSTED_CALLS"]),
        ("inert while EVE_SEARCH_ROUTER is forced off: the routed search", &["EVE_SEARCH_ROUTED_SECS"]),
        ("inert while EVE_SEARCH_ROUTER and EVE_WEB_READER are forced off: Firecrawl (its key is removed too)",
         &["EVE_FIRECRAWL_PER_DAY", "EVE_FIRECRAWL_USD"]),
        ("inert while EVE_FIRETV is forced off: the TV's adb", &["EVE_ADB"]),
        ("inert while EVE_SCENE is forced off: the room camera",
         &["EVE_SCENE_INTERVAL_SECS", "EVE_SCENE_MODEL", "EVE_SCENE_STALE_SECS"]),
        ("inert while EVE_EXPLORE is forced off: the robot's exploring and its notes",
         &["EVE_EXPLORE_AFTER_MIN", "EVE_EXPLORE_CYCLE_MIN", "EVE_NOTES_MODEL", "EVE_NOTES_PER_HOUR"]),
        ("inert while EVE_LEARNING is forced off: the learning server", &["EVE_LEARNING_PORT"]),
        ("inert while EVE_PHONE_EXPRESSIONS is forced off: the phone's faces",
         &["EVE_EXPRESSIONS_FILE", "EVE_EXPRESSION_BACKEND", "EVE_EXPRESSION_MODEL"]),
        ("inert while EVE_PHONE_LOCATION is forced off: the phone's location", &["EVE_PHONE_LOCATION_FILE"]),
        ("inert while EVE_PHONE_LOCATION is forced off: the places history is of the phone's fixes",
         &["EVE_PLACES", "EVE_PLACES_FILE", "EVE_PLACES_DAYS"]),
        ("inert while EVE_AGENTCARD is forced off: AgentCard",
         &["EVE_AGENTCARD_ADDRESS_FILE", "EVE_AGENTCARD_TIMEOUT_SECS"]),
        ("inert while EVE_AMERICA is forced off: America", &["EVE_AMERICA_JEV", "EVE_AMERICA_TRANSPORT"]),
        ("inert while EVE_CHIEF is forced off: the chief of staff",
         &["EVE_CHIEF_ASSESS_EFFORT", "EVE_CHIEF_ASSESS_MODEL", "EVE_CHIEF_DIR", "EVE_CHIEF_PUSH",
           "EVE_CHIEF_REPLAN"]),
        ("inert while EVE_MEET is forced off: the Meet notetaker",
         &["EVE_MEET_GUEST_NAME", "EVE_MEET_LOBBY_MINUTES", "EVE_MEET_MAX_HOURS", "EVE_MEET_PROFILE",
           "EVE_MEET_QUIET", "EVE_MEET_SUMMARY_MODEL"]),
        ("inert while EVE_UPDATES is forced off: the Updates pane",
         &["EVE_UPDATES_EVERY_MIN", "EVE_UPDATES_MODEL", "EVE_UPDATES_COMPOSIO"]),
        // read only by code lite imports and never runs
        ("not run in lite: Claude Code's sessions, launches and permission questions; lite's brain lends none \
(no sessions, a launcher that refuses) and offers no coding tool",
         &["EVE_CLAUDE_CONFIG_DIRS", "EVE_CLAUDE_TERMINAL", "EVE_CODE_AREAS", "EVE_CODE_ROOT", "EVE_TELEGRAM_ASK"]),
        ("not run in lite: the Claude Code hooks' audit log (lite installs no hooks)", &["EVE_BRIDGE_AUDIT"]),
        ("not run in lite: full eve's daemon options (the robot's serial port, saved camera frames), read as the \
CLI's defaults; lite's daemon takes neither",
         &["EVE_SERIAL_PORT", "EVE_SAVE_FRAMES"]),
        (FULL_DAEMON,
         &["EVE_CJK_TARGET", "EVE_COMPUTER", "EVE_HEAD_MODEL", "EVE_OWNER_PRINTS", "EVE_OWNER_THRESHOLD",
           "EVE_PHOTOS_MAX", "EVE_PHOTOS_MAX_MB", "EVE_FOLLOW_SPEAKER", "EVE_PRESENCE_FILE", "EVE_KEY_METHOD",
           "EVE_KEY_SOURCE", "EVE_*_FILE"]),
    ];
    for (reason, names) in groups {
        entries.extend(why(reason, names));
    }
    // a dict literal with ** unpacking: a later duplicate replaces the value and keeps the first position
    let mut out = IndexMap::new();
    for (name, reason) in entries {
        out.insert(name, reason);
    }
    out
});

// ---- the tools eve lite has (docs/plans/eve-lite.md M2) ---------------------------------------------------------

pub const LITE_TOOL_NAMES: &[&str] = &[
    "remember", "think_hard",
    "recent_messages", "web_search", "science_search",
    "memory_search", "memory_read", "forget_preview", "forget_apply", "recent_conversation",
    "watch_add", "watch_list", "watch_remove", "watch_pause", "watch_resume", "watch_set_end",
];
/// Composio Connect's meta tools, as its server names them.
pub const LITE_TOOL_PREFIXES: &[&str] = &["COMPOSIO_"];

/// Python truthiness of a JSON value (None, false, 0, "", [] and {} are false).
fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// A tool definition's name: Responses-style `{"name"}`, chat-style `{"function": {"name"}}`, or a hosted tool's
/// `type` (`{"type": "web_search"}`). "" when it has none.
pub fn tool_name(tool: &Value) -> String {
    let Some(map) = tool.as_object() else {
        return String::new();
    };
    let mut name = map.get("name");
    if !truthy(name) {
        if let Some(Value::Object(function)) = map.get("function") {
            name = function.get("name");
        }
    }
    if !truthy(name) && map.get("type") != Some(&Value::String("function".to_string())) {
        name = map.get("type");
    }
    match name {
        Some(Value::String(s)) => s.clone(),
        _ => String::new(),
    }
}

/// Whether eve lite has the tool `name`: one of LITE_TOOL_NAMES, or a name starting with a LITE_TOOL_PREFIXES entry
/// (and longer than it).
pub fn lite_tool_allowed(name: &str) -> bool {
    LITE_TOOL_NAMES.contains(&name)
        || LITE_TOOL_PREFIXES.iter().any(|p| name.starts_with(p) && name.chars().count() > p.chars().count())
}

/// [`lite_tool_allowed`] for a value of any kind (Python's `Any`): only a string can be a tool's name.
pub fn lite_tool_allowed_value(name: &Value) -> bool {
    match name {
        Value::String(s) => lite_tool_allowed(s),
        _ => false,
    }
}

/// The tools eve lite has, in their order and as they are (the same objects): the rest are left out.
pub fn filter_tools<'a, I>(tools: I) -> Vec<&'a Value>
where
    I: IntoIterator<Item = &'a Value>,
{
    tools.into_iter().filter(|tool| lite_tool_allowed(&tool_name(tool))).collect()
}

/// [`filter_tools`] for owned definitions: the same tools, moved, in their order.
pub fn filter_tools_owned(tools: Vec<Value>) -> Vec<Value> {
    tools.into_iter().filter(|tool| lite_tool_allowed(&tool_name(tool))).collect()
}

