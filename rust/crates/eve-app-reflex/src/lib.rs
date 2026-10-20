//! Open an app without Codex: task_router's launch reflex in front of the Codex computer agent.
//!
//! Port of `eve_bridge/app_reflex.py`.
//!
//! Since computer use moved to Codex (codex_computer.py), every request ("open Spotify" included) starts a Codex
//! turn: a process, a thread, a model, tens of seconds. A quarter of the owner's requests are nothing but a launch
//! (task_router.py's own count), and those need no model at all: `open -a Spotify`.
//!
//! [`ReflexFirstAgent`] wraps whichever agent the daemon builds and keeps its run/steer/cancel/status contract, so
//! the voice and the Telegram door both get it with no change of their own. The wrapped agent is built only when
//! the reflex declines (`make_inner`), so an app launch never spends the warm Codex agent codex_warm.py keeps
//! ready; `on_done` tells it to warm the next one after a Codex task. Before handing the goal over it asks
//! `task_router::classify`:
//!
//! * the rules first (code, microseconds): "open Spotify", "pull up Notes for me";
//! * Jev only for wording the rules do not know ("Notes, please."), with the same gates task_router ships
//!   (`typed_ask::make_jev_request_asker`), via `EVE_ROUTER_MODEL=jev` and the route in `EVE_JEV_ROUTE`
//!   (OpenRouter on this Mac). Jev can only name an installed app.
//!
//! Quitting (owner, 2026-09-23) takes the same path: only the explicit word "quit", a graceful quit (the app's own
//! "save changes?" dialog still appears; never a force quit). "quit all" quits every regular app except the ones
//! in [`QUIT_ALL_KEEP`] (the terminals Claude Code runs in, Claude, ChatGPT/Codex (the computer use eve hands off
//! to), eve's own app, Finder) plus `EVE_QUIT_ALL_KEEP`. Chosen by tools/route_eval.py --quit on a blind holdout:
//! rules then Jev, 45 of 45, 0 unsafe.
//!
//! Only a request the plan says is COMPLETE (a bare launch of an installed app, nothing after it) is opened here.
//! Anything else, a failed `open`, a Jev error, or `EVE_REFLEXES=0`: the wrapped agent runs the goal exactly as
//! before.

use std::any::Any;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::future::BoxFuture;
use parking_lot::Mutex;
use regex::Regex;
use serde_json::{json, Map, Value};
use tracing::{info, warn};

pub use eve_agent_contract::AgentEvent;
use eve_desk_lane::DeskLane;
use eve_desktop_lanes::task_router::{self, Plan};
use eve_desktop_lanes::{casefold, py_split, py_strip};

/// The process environment, or an explicit one (`environ` in Python; `None` reads `os.environ`).
pub type Environ<'a> = Option<&'a HashMap<String, String>>;

/// `env.get(name, "")`, as a string.
fn env_get(environ: Environ<'_>, name: &str) -> String {
    match environ {
        Some(env) => env.get(name).cloned().unwrap_or_default(),
        None => std::env::var_os(name).map(|v| v.to_string_lossy().into_owned()).unwrap_or_default(),
    }
}

/// `EVE_REFLEXES`: unset or empty is `task_router.REFLEX_DEFAULT`; "0", "false", "no", "off" turn it off.
pub fn reflexes_on(environ: Environ<'_>) -> bool {
    let raw = py_strip(&env_get(environ, "EVE_REFLEXES")).to_lowercase();
    if raw.is_empty() {
        task_router::REFLEX_DEFAULT
    } else {
        !matches!(raw.as_str(), "0" | "false" | "no" | "off")
    }
}

// A task's goal is the owner's words for THIS request ("Use Google search to open it up"), written by the door's
// model; a link the owner sent in an earlier message is not in it (production, 2026-09-24: a Google Maps link,
// then "open it up", and the Chrome lane got the words without the link). When the goal refers back and carries
// no link of its own, the links from the owner's recent messages go with it (`with_referenced_links`).
pub static LINK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?i)https?://[^\s<>"'`]+"#).expect("LINK"));
pub static REFERS_BACK: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)\b(that|this|the|same)\s+(link|url|page|site|website|place|address|location|map|pin|article|video|listing|post)\b",
        r"|\b(open|visit|load|show|check|search|look up|go to|read|click|pull up|bring up|navigate to)\s+",
        r"(it|that|this|them|those|these)\b",
    ))
    .expect("REFERS_BACK")
});
/// How far back a "that link" may reach.
pub const RECENT_OWNER_MESSAGES: usize = 4;
pub const MAX_REFERENCED_LINKS: usize = 3;

/// The http(s) links in a text, in order, without trailing punctuation, deduplicated.
pub fn links_in(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in LINK.find_iter(text) {
        let url = m.as_str().trim_end_matches(|c| ".,;:!?)]}".contains(c)).to_string();
        if !out.contains(&url) {
            out.push(url);
        }
    }
    out
}

pub fn refers_back(goal: &str) -> bool {
    REFERS_BACK.is_match(goal)
}

/// `goal` plus the links from the owner's recent messages (oldest to newest; the last RECENT_OWNER_MESSAGES are
/// read, newest link first) when the goal refers back to something ("open it", "that link") and has no link of
/// its own. Otherwise `goal` unchanged.
pub fn with_referenced_links<I, S>(goal: &str, recent_owner_texts: I) -> String
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    if goal.is_empty() || !links_in(goal).is_empty() || !refers_back(goal) {
        return goal.to_string();
    }
    let texts: Vec<S> = recent_owner_texts.into_iter().collect();
    let start = texts.len().saturating_sub(RECENT_OWNER_MESSAGES);
    let mut found: Vec<String> = Vec::new();
    for text in texts[start..].iter().rev() {
        for url in links_in(text.as_ref()) {
            if !found.contains(&url) {
                found.push(url);
            }
        }
    }
    if found.is_empty() {
        return goal.to_string();
    }
    found.truncate(MAX_REFERENCED_LINKS);
    format!(
        "{goal}\n\n[note] The owner's recent messages included {}, which the request refers to: {}",
        if found.len() == 1 { "this link" } else { "these links, newest first" },
        found.join(" ")
    )
}

/// `urlsplit(url).hostname` for a link `LINK` found: the netloc's host, lower-cased, without user info or port;
/// `None` when empty.
fn url_hostname(url: &str) -> Option<String> {
    let rest = &url[url.find("://")? + 3..];
    let netloc = &rest[..rest.find(['/', '?', '#']).unwrap_or(rest.len())];
    let hostinfo = netloc.rsplit_once('@').map_or(netloc, |(_, h)| h);
    let host = match hostinfo.split_once('[') {
        Some((_, bracketed)) => bracketed.split_once(']').map_or(bracketed, |(h, _)| h),
        None => hostinfo.split_once(':').map_or(hostinfo, |(h, _)| h),
    };
    if host.is_empty() {
        return None;
    }
    // urllib keeps an IPv6 zone id's case: only the part before "%" is lower-cased
    Some(match host.split_once('%') {
        Some((addr, zone)) => format!("{}%{zone}", addr.to_lowercase()),
        None => host.to_lowercase(),
    })
}

/// What a goal looks like, for the log, without the owner's words: its length, its links' hosts, whether it refers
/// back to something.
pub fn goal_shape(goal: &str) -> String {
    let hosts: Vec<String> = links_in(goal).iter().map(|u| url_hostname(u).unwrap_or_else(|| "?".into())).collect();
    let mut out = format!("{} words, {} link(s)", py_split(goal).count(), hosts.len());
    if !hosts.is_empty() {
        out.push_str(&format!(" ({})", hosts.join(", ")));
    }
    if refers_back(goal) {
        out.push_str(", refers back");
    }
    out
}

pub const QUIT_ALL_KEEP: [&str; 14] = [
    "finder",
    "warp",
    "stable",
    "terminal",
    "iterm2",
    "iterm",
    "ghostty",
    "cmux",
    "claude",
    "chatgpt",
    "codex",
    "stackchannotes",
    "codex computer use",
    "skycomputeruseservice",
];
/// The longest wait for apps to go; checked every QUIT_POLL_SECS, done when they are.
pub const QUIT_SETTLE_SECS: f64 = 2.0;
pub const QUIT_POLL_SECS: f64 = 0.25;

/// QUIT_ALL_KEEP plus the comma-separated names in `EVE_QUIT_ALL_KEEP`, case-folded.
pub fn quit_keep(environ: Environ<'_>) -> HashSet<String> {
    let mut keep: HashSet<String> = QUIT_ALL_KEEP.iter().map(|s| s.to_string()).collect();
    for part in env_get(environ, "EVE_QUIT_ALL_KEEP").split(',') {
        let part = py_strip(part);
        if !part.is_empty() {
            keep.insert(casefold(part));
        }
    }
    keep
}

/// `model(goal, installed apps) -> app or ""`; an `Err` is an abstention (Python's raised exception).
pub type Asker = Arc<dyn Fn(&str, &[String]) -> anyhow::Result<String> + Send + Sync>;

/// `str(env.get("EVE_ROUTER_MODEL", "")).strip().lower() == "jev"`.
fn router_model_is_jev(environ: Environ<'_>) -> bool {
    py_strip(&env_get(environ, "EVE_ROUTER_MODEL")).to_lowercase() == "jev"
}

/// `jev.route_config(env)` then `jev.make_predict(url, key, model, timeout_s=2.0)`, as the typed askers' seam.
fn jev_predict(
    environ: Environ<'_>,
) -> Result<impl Fn(&Value, &Value) -> anyhow::Result<Value> + Send + Sync + 'static, eve_jev::JevError> {
    let (url, key, model) = match environ {
        Some(env) => eve_jev::route_config(env, "")?,
        None => eve_jev::route_config(&eve_jev::ProcessEnv, "")?,
    };
    let options = eve_jev::PredictOptions { timeout_s: 2.0, ..Default::default() };
    Ok(eve_jev::make_predict(&url, &key, &model, options).anyhow_fn())
}

/// Jev for quit wording the rules do not know, under the same switch as [`jev_asker`].
pub fn jev_quit_asker(environ: Environ<'_>) -> Option<Asker> {
    if !router_model_is_jev(environ) {
        return None;
    }
    match jev_predict(environ) {
        Ok(predict) => {
            let ask = eve_typed_ask::make_jev_quit_asker(predict, eve_typed_ask::perf_counter);
            Some(Arc::new(move |goal: &str, apps: &[String]| Ok(ask(goal, apps))))
        }
        Err(_) => {
            warn!("app-reflex: Jev quit is off (JevError)");
            None
        }
    }
}

/// Jev for unfamiliar wording, or None when EVE_ROUTER_MODEL is not jev or no key is set.
pub fn jev_asker(environ: Environ<'_>) -> Option<Asker> {
    if !router_model_is_jev(environ) {
        return None;
    }
    match jev_predict(environ) {
        Ok(predict) => {
            let ask = eve_typed_ask::make_jev_request_asker(predict, eve_typed_ask::perf_counter);
            Some(Arc::new(move |goal: &str, apps: &[String]| Ok(ask(goal, apps))))
        }
        // no Jev is "rules only", never a failure
        Err(_) => {
            warn!("app-reflex: Jev is off (JevError)");
            None
        }
    }
}

// ---- osascript: quitting apps -------------------------------------------------------------------------------

/// The AppleScript runner quitting goes through (Python's `_osascript` and `_running`, which the tests patch).
#[async_trait]
pub trait Osascript: Send + Sync {
    /// `osascript -e line …`: (ok, stdout when ok else stderr), decoded and stripped. An `Err` is a raised
    /// exception (no osascript binary).
    async fn osascript(&self, lines: &[String]) -> anyhow::Result<(bool, String)>;

    /// Whether the app called `name` is running.
    async fn running(&self, name: &str) -> anyhow::Result<bool> {
        let (ok, out) = self.osascript(&[format!("return application {} is running", quote(name))]).await?;
        Ok(ok && out == "true")
    }
}

/// The real `osascript`.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealOsascript;

#[async_trait]
impl Osascript for RealOsascript {
    async fn osascript(&self, lines: &[String]) -> anyhow::Result<(bool, String)> {
        let mut cmd = tokio::process::Command::new("osascript");
        for line in lines {
            cmd.arg("-e").arg(line);
        }
        let out = cmd.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).output().await?;
        let ok = out.status.success();
        let bytes = if ok { &out.stdout } else { &out.stderr };
        Ok((ok, py_strip(&String::from_utf8_lossy(bytes)).to_string()))
    }
}

/// An AppleScript string literal.
pub fn quote(name: &str) -> String {
    format!("\"{}\"", name.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The first `n` characters (Python's `s[:n]`).
fn take_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// Wait until every app in `names` has quit or `settle` seconds pass; the ones still running.
async fn gone(osa: &dyn Osascript, names: &[String], settle: f64) -> anyhow::Result<Vec<String>> {
    let deadline = Instant::now() + Duration::try_from_secs_f64(settle.max(0.0)).unwrap_or(Duration::MAX);
    let mut left: Vec<String> = names.to_vec();
    while !left.is_empty() {
        let mut still = Vec::new();
        for n in left {
            if osa.running(&n).await? {
                still.push(n);
            }
        }
        left = still;
        if left.is_empty() || Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_secs_f64(QUIT_POLL_SECS)).await;
    }
    Ok(left)
}

/// The three lines that quit one app the way Cmd+Q does, without waiting on it.
fn quit_lines(name: &str) -> [String; 3] {
    [
        "ignoring application responses".to_string(),
        format!("tell application {} to quit", quote(name)),
        "end ignoring".to_string(),
    ]
}

/// Quit one app the way Cmd+Q does, without waiting on it. The sentence for the owner.
pub async fn quit_app(name: &str, settle: f64) -> anyhow::Result<String> {
    quit_app_with(&RealOsascript, name, settle).await
}

/// [`quit_app`] through another AppleScript runner.
pub async fn quit_app_with(osa: &dyn Osascript, name: &str, settle: f64) -> anyhow::Result<String> {
    if !osa.running(name).await? {
        return Ok(format!("{name} isn't open."));
    }
    let (ok, err) = osa.osascript(&quit_lines(name)).await?;
    if !ok {
        return Ok(format!("I couldn't quit {name}: {}", take_chars(&err, 120)));
    }
    if !gone(osa, &[name.to_string()], settle).await?.is_empty() {
        return Ok(format!("{name} didn't quit. It may be asking to save, or it declined; it's still open."));
    }
    Ok(format!("Quit {name}."))
}

/// Quit every regular app except `keep` (process or app names, case-folded).
pub async fn quit_all(keep: &HashSet<String>, settle: f64) -> anyhow::Result<String> {
    quit_all_with(&RealOsascript, keep, settle).await
}

/// [`quit_all`] through another AppleScript runner.
pub async fn quit_all_with(osa: &dyn Osascript, keep: &HashSet<String>, settle: f64) -> anyhow::Result<String> {
    let (ok, out) = osa
        .osascript(&["tell application \"System Events\" to get name of every application process whose background only is false"
            .to_string()])
        .await?;
    if !ok {
        return Ok(format!("I couldn't list the open apps: {}", take_chars(&out, 120)));
    }
    let names: Vec<String> = out.split(',').map(py_strip).filter(|n| !n.is_empty()).map(str::to_string).collect();
    let targets: Vec<String> = names.iter().filter(|n| !keep.contains(&casefold(n))).cloned().collect();
    let kept: Vec<String> = names
        .iter()
        .filter(|n| keep.contains(&casefold(n)) && n.as_str() != "Finder")
        .map(|n| if n == "stable" { "Warp".to_string() } else { n.clone() })
        .collect();
    if targets.is_empty() {
        let mut said = "Nothing to quit.".to_string();
        if !kept.is_empty() {
            said.push_str(&format!(" Kept {}.", kept.join(", ")));
        }
        return Ok(said);
    }
    for n in &targets {
        osa.osascript(&quit_lines(n)).await?;
    }
    let waiting = gone(osa, &targets, settle).await?;
    let done: Vec<String> = targets.iter().filter(|n| !waiting.contains(n)).cloned().collect();
    let mut said = format!("Quit {} app{}", done.len(), if done.len() != 1 { "s" } else { "" });
    if done.is_empty() {
        said.push('.');
    } else {
        said.push_str(&format!(": {}.", done.join(", ")));
    }
    if !waiting.is_empty() {
        said.push_str(&format!(
            " {} didn't quit (asking to save, or declined); {} still open.",
            waiting.join(", "),
            if waiting.len() == 1 { "it is" } else { "they are" }
        ));
    }
    if !kept.is_empty() {
        said.push_str(&format!(" Kept {}.", kept.join(", ")));
    }
    Ok(said)
}

/// `open -a app`: (ok, the first 200 characters of stderr, stripped).
pub async fn open_app(app: &str) -> anyhow::Result<(bool, String)> {
    let out = tokio::process::Command::new("open")
        .arg("-a")
        .arg(app)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .output()
        .await?;
    Ok((out.status.success(), take_chars(py_strip(&String::from_utf8_lossy(&out.stderr)), 200)))
}

// ---- the wrapper ---------------------------------------------------------------------------------------------

/// The run/steer/cancel/status contract every computer-use agent (Codex, the Chrome lane, the web reader) keeps,
/// and that [`ReflexFirstAgent`] keeps in turn.
#[async_trait]
pub trait Agent: Send + Sync {
    /// Run one task to its final sentence.
    async fn run(&self, goal: &str) -> anyhow::Result<String>;
    /// `cancel(reason=...)`.
    fn cancel(&self, reason: &str);
    /// `steer(text)`: whether the steer was taken.
    fn steer(&self, text: &str) -> bool;
    /// `status()`.
    fn status(&self) -> Map<String, Value>;
    /// `running`; `False` when the agent has none.
    fn running(&self) -> bool {
        false
    }
    /// `provider`; `""` when the agent has none.
    fn provider(&self) -> String {
        String::new()
    }
    /// `discard()`: close a built agent that will not run (a warm Codex agent's app-server). An agent without one
    /// does nothing.
    async fn discard(&self) -> anyhow::Result<()> {
        Ok(())
    }
    /// The concrete agent, for the attributes Python reaches through `__getattr__` (`browser_used`,
    /// `browser_screenshot`, `final` …).
    fn as_any(&self) -> &dyn Any;
}

/// Builds an agent (`make_inner`, `make_auto`, a `bodies` entry).
pub type MakeAgent = Arc<dyn Fn() -> Arc<dyn Agent> + Send + Sync>;
/// `on_event(AgentEvent)`.
pub type OnEvent = Arc<dyn Fn(AgentEvent) + Send + Sync>;
/// `apps()`: the installed apps' names.
pub type Apps = Arc<dyn Fn() -> Vec<String> + Send + Sync>;
/// `opener(app) -> (ok, err)`.
pub type Opener = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<(bool, String)>> + Send + Sync>;
/// `quitter(app) -> sentence`.
pub type Quitter = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;
/// `quit_everything() -> sentence`.
pub type QuitEverything = Arc<dyn Fn() -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;
/// `route_body(goal) -> body name` ("codex", a `bodies` key, or another name for `make_auto`).
pub type RouteBody = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<String>> + Send + Sync>;
/// `desk(goal) -> lane_router.RouteResult as a dict, or None` (desk_lane.DeskLane.attempt).
pub type Desk = Arc<dyn Fn(String) -> BoxFuture<'static, anyhow::Result<Option<Map<String, Value>>>> + Send + Sync>;
/// `on_done()`.
pub type OnDone = Arc<dyn Fn() + Send + Sync>;

/// `desk=lane.attempt`: a desk lane as the wrapper's `desk`.
pub fn desk_from_lane(lane: DeskLane) -> Desk {
    Arc::new(move |goal: String| {
        let lane = lane.clone();
        Box::pin(async move { Ok(lane.attempt(&goal).await) })
    })
}

/// The keyword arguments of `ReflexFirstAgent(...)`; `Default` is Python's defaults.
#[derive(Clone)]
pub struct ReflexOptions {
    /// default `task_router.installed_apps`
    pub apps: Apps,
    pub asker: Option<Asker>,
    /// default [`open_app`]
    pub opener: Opener,
    pub enabled: bool,
    pub on_done: OnDone,
    pub provider: String,
    pub quit_asker: Option<Asker>,
    /// default [`quit_app`] with QUIT_SETTLE_SECS
    pub quitter: Quitter,
    /// default `quit_all(quit_keep())`, the keep list read when it runs
    pub quit_everything: Option<QuitEverything>,
    pub make_auto: Option<MakeAgent>,
    pub route_body: Option<RouteBody>,
    pub bodies: HashMap<String, MakeAgent>,
    pub desk: Option<Desk>,
}

impl Default for ReflexOptions {
    fn default() -> Self {
        ReflexOptions {
            apps: Arc::new(task_router::installed_apps),
            asker: None,
            opener: Arc::new(|app: String| Box::pin(async move { open_app(&app).await })),
            enabled: true,
            on_done: Arc::new(|| {}),
            provider: "codex".to_string(),
            quit_asker: None,
            quitter: Arc::new(|app: String| Box::pin(async move { quit_app(&app, QUIT_SETTLE_SECS).await })),
            quit_everything: None,
            make_auto: None,
            route_body: None,
            bodies: HashMap::new(),
            desk: None,
        }
    }
}

struct State {
    inner: Option<Arc<dyn Agent>>,
    reflex_running: bool,
    cancel_reason: Option<String>,
    body: String,
    provider: String,
    reflexed: Option<String>,
}

/// The wrapped agent's contract, with a bare app launch answered by `open -a`.
pub struct ReflexFirstAgent {
    make_inner: MakeAgent,
    on_event: OnEvent,
    apps: Apps,
    asker: Option<Asker>,
    opener: Opener,
    enabled: bool,
    on_done: OnDone,
    quit_asker: Option<Asker>,
    quitter: Quitter,
    quit_everything: QuitEverything,
    // route_body names the body for a task the reflex declined: a name in `bodies` (e.g. "web",
    // browser_router.py), any other non-Codex name the one `make_auto` body (the Chrome lane), else Codex
    make_auto: Option<MakeAgent>,
    route_body: Option<RouteBody>,
    bodies: HashMap<String, MakeAgent>,
    // desk_lane.DeskLane.attempt: keyword-decided clicks on the frontmost window before any body (desk_lane.py)
    desk: Option<Desk>,
    state: Mutex<State>,
}

/// `finally: self._reflex_running = False`, also when the run is dropped mid-await.
struct ReflexRunning<'a>(&'a Mutex<State>);

impl<'a> ReflexRunning<'a> {
    fn start(state: &'a Mutex<State>) -> Self {
        state.lock().reflex_running = true;
        ReflexRunning(state)
    }
}

impl Drop for ReflexRunning<'_> {
    fn drop(&mut self) {
        self.0.lock().reflex_running = false;
    }
}

/// `finally: self._on_done()`, also when the run is dropped mid-await.
struct DoneOnDrop<'a>(&'a OnDone);

impl Drop for DoneOnDrop<'_> {
    fn drop(&mut self) {
        (self.0)();
    }
}

/// `type(e).__name__` for the log: the Python class an error stands for, never its message.
pub fn exception_name(e: &anyhow::Error) -> String {
    for cause in e.chain() {
        if cause.downcast_ref::<eve_jev::JevError>().is_some() {
            return "JevError".into();
        }
        if let Some(f) = cause.downcast_ref::<eve_jev::PredictFailure>() {
            return f.kind().to_string();
        }
        if let Some(p) = cause.downcast_ref::<eve_typed_ask::PredictError>() {
            return p.kind.clone();
        }
        if let Some(l) = cause.downcast_ref::<eve_desktop_lanes::LaneError>() {
            return l.kind.clone();
        }
        if let Some(io) = cause.downcast_ref::<std::io::Error>() {
            return match io.kind() {
                std::io::ErrorKind::TimedOut => "TimeoutError",
                std::io::ErrorKind::NotFound => "FileNotFoundError",
                std::io::ErrorKind::PermissionDenied => "PermissionError",
                std::io::ErrorKind::ConnectionRefused => "ConnectionRefusedError",
                std::io::ErrorKind::ConnectionReset => "ConnectionResetError",
                std::io::ErrorKind::BrokenPipe => "BrokenPipeError",
                _ => "OSError",
            }
            .into();
        }
        if cause.downcast_ref::<tokio::time::error::Elapsed>().is_some() {
            return "TimeoutError".into();
        }
    }
    "Exception".into()
}

/// Python truthiness of a JSON value.
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Python `str()` of a JSON value.
fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(true) => "True".into(),
        Value::Bool(false) => "False".into(),
        Value::Null => "None".into(),
        other => other.to_string(),
    }
}

impl ReflexFirstAgent {
    pub fn new(make_inner: MakeAgent, on_event: OnEvent, options: ReflexOptions) -> Self {
        let quit_everything: QuitEverything = options.quit_everything.unwrap_or_else(|| {
            Arc::new(|| Box::pin(async move { quit_all(&quit_keep(None), QUIT_SETTLE_SECS).await }))
        });
        ReflexFirstAgent {
            make_inner,
            on_event,
            apps: options.apps,
            asker: options.asker,
            opener: options.opener,
            enabled: options.enabled,
            on_done: options.on_done,
            quit_asker: options.quit_asker,
            quitter: options.quitter,
            quit_everything,
            make_auto: options.make_auto,
            route_body: options.route_body,
            bodies: options.bodies,
            desk: options.desk,
            state: Mutex::new(State {
                inner: None,
                reflex_running: false,
                cancel_reason: None,
                body: "codex".to_string(),
                provider: options.provider,
                reflexed: None,
            }),
        }
    }

    /// The reflex or lane is working, or the wrapped agent says it is running.
    pub fn running(&self) -> bool {
        let state = self.state.lock();
        state.reflex_running || state.inner.as_ref().is_some_and(|a| a.running())
    }

    /// Who took the task: "codex", a `bodies` name, the `make_auto` name, or "desk".
    pub fn body(&self) -> String {
        self.state.lock().body.clone()
    }

    pub fn provider(&self) -> String {
        self.state.lock().provider.clone()
    }

    /// The app opened (or quit; "*" for all, "desk" for the desk lane) here, for the log and the tests.
    pub fn reflexed(&self) -> Option<String> {
        self.state.lock().reflexed.clone()
    }

    /// The wrapped agent once the task was handed over (Python's `__getattr__` forwards to it).
    pub fn inner(&self) -> Option<Arc<dyn Agent>> {
        self.state.lock().inner.clone()
    }

    fn inner_or_cancel(&self, reason: &str) -> Option<Arc<dyn Agent>> {
        let mut state = self.state.lock();
        match state.inner.clone() {
            Some(inner) => Some(inner),
            None => {
                // before the handoff: the wrapped agent never starts
                state.cancel_reason = Some(reason.to_string());
                None
            }
        }
    }

    pub fn cancel(&self, reason: &str) {
        if let Some(inner) = self.inner_or_cancel(reason) {
            inner.cancel(reason);
        }
    }

    pub fn steer(&self, text: &str) -> bool {
        self.inner().is_some_and(|inner| inner.steer(text))
    }

    pub fn status(&self) -> Map<String, Value> {
        match self.inner() {
            Some(inner) => inner.status(),
            None => {
                let running = self.running();
                let state = self.state.lock();
                let reflex = state.reflexed.clone().map_or(Value::Null, Value::String);
                match json!({"running": running, "provider": state.provider, "goal": "", "reflex": reflex}) {
                    Value::Object(m) => m,
                    _ => Map::new(),
                }
            }
        }
    }

    fn cancelled(&self) -> bool {
        self.state.lock().cancel_reason.is_some()
    }

    fn emit(&self, kind: &str, text: &str) {
        (self.on_event)(AgentEvent::with_text(kind, text));
    }

    /// Codex, unless another body is wired in and the router sends this task there. Never fails.
    async fn choose_body(&self, goal: &str) -> Arc<dyn Agent> {
        if let Some(route_body) = &self.route_body {
            if self.make_auto.is_some() || !self.bodies.is_empty() {
                let body = match route_body(goal.to_string()).await {
                    Ok(body) => body,
                    // a router that fails keeps the task with Codex
                    Err(e) => {
                        warn!("app-reflex: the body router failed ({}); Codex takes it", exception_name(&e));
                        "codex".to_string()
                    }
                };
                let make = self
                    .bodies
                    .get(&body)
                    .or(if !body.is_empty() && body != "codex" { self.make_auto.as_ref() } else { None });
                if let Some(make) = make {
                    let agent = make();
                    let provider = agent.provider();
                    let provider = if provider.is_empty() { body.clone() } else { provider };
                    {
                        let mut state = self.state.lock();
                        state.body = body;
                        state.provider = provider.clone();
                    }
                    info!("app-reflex: {} takes this task", provider);
                    return agent;
                }
            }
        }
        (self.make_inner)()
    }

    /// The plan for a goal: the installed apps and task_router.classify, off the async runtime (Jev is a blocking
    /// HTTP call).
    async fn plan(&self, goal: &str) -> anyhow::Result<Plan> {
        let apps_fn = Arc::clone(&self.apps);
        let apps = tokio::task::spawn_blocking(move || apps_fn()).await?;
        let (goal, asker, quit_asker) = (goal.to_string(), self.asker.clone(), self.quit_asker.clone());
        // classify is pure unless Jev is asked; Jev is a blocking HTTP call, so off the loop.
        let plan = tokio::task::spawn_blocking(move || {
            let model: Option<task_router::RouterModel<'_>> = asker.as_deref().map(|a| a as task_router::RouterModel<'_>);
            let quit_model: Option<task_router::RouterModel<'_>> =
                quit_asker.as_deref().map(|a| a as task_router::RouterModel<'_>);
            task_router::classify(&goal, "", &apps, model, quit_model)
        })
        .await?;
        Ok(plan)
    }

    /// The reflex: `Some(final)` when it answered the request, `None` when it declined.
    async fn reflex(&self, goal: &str) -> anyhow::Result<Option<String>> {
        let plan = self.plan(goal).await?;
        if plan.complete() && (plan.kind == "quit" || plan.kind == "quit_all") {
            let t0 = Instant::now();
            let final_ =
                if plan.kind == "quit" { (self.quitter)(plan.app.clone()).await? } else { (self.quit_everything)().await? };
            self.state.lock().reflexed = Some(if plan.app.is_empty() { "*".to_string() } else { plan.app.clone() });
            info!(
                "app-reflex: {} {} in {:.2} s ({})",
                plan.kind,
                if plan.app.is_empty() { "all" } else { plan.app.as_str() },
                t0.elapsed().as_secs_f64(),
                plan.reasons.join("; ")
            );
            self.emit("started", goal);
            self.emit("final", &final_);
            return Ok(Some(final_));
        }
        if plan.kind == "launch" && plan.complete() && !plan.app.is_empty() {
            let t0 = Instant::now();
            let (ok, err) = (self.opener)(plan.app.clone()).await?;
            if ok {
                self.state.lock().reflexed = Some(plan.app.clone());
                info!(
                    "app-reflex: opened {} in {:.2} s ({})",
                    plan.app,
                    t0.elapsed().as_secs_f64(),
                    plan.reasons.join("; ")
                );
                let final_ = format!("Opened {}.", plan.app);
                self.emit("started", goal);
                self.emit("final", &final_);
                return Ok(Some(final_));
            }
            warn!("app-reflex: open -a {} failed ({}); Codex takes it", plan.app, err);
        }
        Ok(None)
    }

    fn stopped(&self) -> String {
        self.emit("cancelled", "Stopped.");
        "Stopped.".to_string()
    }

    /// `goal` is the owner's request and the only text the reflex and the body router read. `note` is the door's
    /// own instruction to the body (telegram.TASK_FILE_HINT), added after the route is chosen: read with the goal,
    /// its "open it in a browser" sent every Telegram task to the Chrome lane and kept "open Spotify" from being a
    /// bare launch (production, 2026-09-26).
    pub async fn run(&self, goal: &str, note: &str) -> anyhow::Result<String> {
        let mut note = note.to_string();
        if self.enabled {
            let _running = ReflexRunning::start(&self.state);
            match self.reflex(goal).await {
                Ok(Some(final_)) => return Ok(final_),
                Ok(None) => {}
                // the reflex never costs the request
                Err(e) => warn!("app-reflex: skipped ({})", exception_name(&e)),
            }
        }
        if self.cancelled() {
            return Ok(self.stopped());
        }
        if let Some(desk) = &self.desk {
            let lane = {
                let _running = ReflexRunning::start(&self.state);
                match desk(goal.to_string()).await {
                    Ok(lane) => lane,
                    // the lane never costs the request
                    Err(e) => {
                        warn!("app-reflex: the desk lane failed ({}); a body takes it", exception_name(&e));
                        None
                    }
                }
            };
            if let Some(lane) = &lane {
                let status = lane.get("status").and_then(Value::as_str);
                if status == Some("complete") && truthy(lane.get("sentence")) {
                    let final_ = lane.get("sentence").map(py_str).unwrap_or_default();
                    {
                        let mut state = self.state.lock();
                        state.body = "desk".into();
                        state.provider = "desk lane".into();
                        state.reflexed = Some("desk".into());
                    }
                    self.emit("started", goal);
                    self.emit("final", &final_);
                    return Ok(final_);
                }
                if status == Some("partial") {
                    note = eve_desk_lane::codex_note(lane) + &note;
                }
            }
            if self.cancelled() {
                return Ok(self.stopped());
            }
        }
        let body = self.choose_body(goal).await;
        // The router is an await (Jev, about 2 s) and a Stop in it only set the cancel reason, since there was no
        // body to forward it to: look again, or the task starts anyway.
        let handed = {
            let mut state = self.state.lock();
            if state.cancel_reason.is_some() {
                false
            } else {
                state.inner = Some(Arc::clone(&body));
                true
            }
        };
        if !handed {
            // The body was built and will not run: a warm Codex agent's app-server is closed, and on_done tells
            // codex_warm.py the hand-out is over, as a run that ended would.
            let discarded = {
                let _done = DoneOnDrop(&self.on_done);
                body.discard().await
            };
            discarded?;
            return Ok(self.stopped());
        }
        // The goal's shape, never its words: enough to tell later whether a link reached the body.
        info!("app-reflex: {} gets a goal of {}", self.provider(), goal_shape(goal));
        let _done = DoneOnDrop(&self.on_done);
        body.run(&format!("{goal}{note}")).await
    }
}

#[async_trait]
impl Agent for ReflexFirstAgent {
    async fn run(&self, goal: &str) -> anyhow::Result<String> {
        ReflexFirstAgent::run(self, goal, "").await
    }

    fn cancel(&self, reason: &str) {
        ReflexFirstAgent::cancel(self, reason)
    }

    fn steer(&self, text: &str) -> bool {
        ReflexFirstAgent::steer(self, text)
    }

    fn status(&self) -> Map<String, Value> {
        ReflexFirstAgent::status(self)
    }

    fn running(&self) -> bool {
        ReflexFirstAgent::running(self)
    }

    fn provider(&self) -> String {
        ReflexFirstAgent::provider(self)
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

#[cfg(test)]
mod tests;
