//! A port of `self_context.py`, the whole module.
//!
//! Self-context: what eve runs on right now, read from its own code and settings, for its brains to read.
//!
//! The owner texted "what do you use for search" on 2026-09-29 and eve answered from a memory record naming an
//! engine it had not used since 2026-09-24. Nothing in any prompt described eve's own wiring, so a question about it
//! was answered from memory, and memory drifts. This module writes a short block, one line per area, from the same
//! ``configured()`` calls the daemon builds its doors from, so it cannot drift from what runs: the models per door,
//! web search (the providers usable here when the search router routes, else websearch's one engine), tasks on the
//! Mac, the watcher's reading ladder, the lights by brand, the memory stores, the doors that are on, the git commit
//! the code runs from, Canvas, the owner's apps and file sharing, the chief of staff and purchases, each only while
//! on where the Python says so.
//!
//! eve lite (EVE_PROFILE=lite) has a block of its own (``lite_lines``): a part eve lite does not have gets no line
//! at all, and no git is started for the commit line.
//!
//! No value is typed here: every model, engine and brand comes from the module that uses it, and a component that
//! is off says so. Names are only ever the ones that are live now: never a retired one, not even as "not X".
//!
//! Each door's config carries the block (``about``), filled once in its ``configured()``; a config built by hand has
//! ``about=""``. Target: under BUDGET characters, the rule sentence included.

#[allow(unused_imports)]
use crate::prelude::*;

use std::cell::Cell;
use std::collections::BTreeSet;
use std::io::Read as _;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use eve_web::{search_router, web_reader, websearch};

// py: self_context.py:58 `log` (Logger): tracing, target `eve_brain::self_context`.

/// py: self_context.py:60 `BUDGET`
/// characters for the whole block: every turn of every door pays for it
pub const BUDGET: usize = 1450;

/// py: self_context.py:64 `HEADER`
pub const HEADER: &str = "

Your live setup, from eve's code and settings at start:";

/// py: self_context.py:67 `RULE`
/// Scoped to what the list is (owner, 2026-09-29): the source of truth for the models, services and providers eve
/// runs on. What eve can do is its tool list, so a question about a tool is never answered "I do not know" here.
pub const RULE: &str = "This list is the source of truth for the models, services and providers you run on, over memory and past chats: if it does not name one, say you do not know; never guess. What you can do is your tools.";

/// py: self_context.py:70 `LIGHT_BRANDS`
/// How each brand of light is called by the owner (lights.KINDS is the set; "triones" is HappyLighting's protocol).
pub const LIGHT_BRANDS: &[(&str, &str)] = &[("govee", "Govee"), ("wiz", "WiZ"), ("triones", "HappyLighting"), ("tuya", "Sylvania (Tuya)")];

/// py: self_context.py:73 `PROVIDER_WORDS`
/// search_router.PROVIDERS in words ("perplexity" is named by websearch's engine, provider_words). A name not here
/// is shown as the router gives it.
pub const PROVIDER_WORDS: &[(&str, &str)] = &[("tinyfish", "TinyFish"), ("firecrawl", "Firecrawl")];

/// py: self_context.py:75 `_QUIET`
/// The modules whose configured() this reads: their warnings were already logged once when the daemon built them.
pub(crate) const QUIET: &[&str] = &["telegram", "voice_agent", "think", "miniapp", "websearch", "web_reader", "watch", "lights", "ears", "meet", "transcripts", "mem0_memory", "second_brain", "recall", "search_router", "jev", "tinyfish", "firecrawl", "browser_router", "computer_agent", "chief", "canvas", "agentcard", "apps_mcp", "composio_tools", "share"];

thread_local! {
    /// py: self_context.py:80 `_building`: a configured() that builds the block reads other configured()s: no loop.
    static BUILDING: Cell<bool> = const { Cell::new(false) };
}

/// The web crate's context reading `env` (websearch, web_reader, watch and the search router take one).
pub(crate) fn web_ctx(env: &Environ) -> eve_web::Ctx {
    eve_web::Ctx::global().clone().with_env(env.clone())
}

/// py: self_context.py:83 `def _join(words: list[str]) -> str:`
pub(crate) fn join(words: &[String]) -> String {
    match words {
        [] => String::new(),
        [one] => one.clone(),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}

/// Python's `str.capitalize()`: the first character upper case, the rest lower.
fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars.flat_map(char::to_lowercase)).collect(),
        None => String::new(),
    }
}

// ---- _quiet: drop those modules' own log records while the block is built ------------------------------------

/// The tracing targets of the modules in [`QUIET`] (Python's `eve_bridge.<name>` loggers): the crate or module
/// each was ported to.
fn quiet_targets(name: &str) -> &'static [&'static str] {
    match name {
        "telegram" => &["eve_brain::telegram"],
        "voice_agent" => &["eve_brain::voice_agent"],
        "think" => &["eve_brain::think"],
        "miniapp" => &["eve_brain::miniapp"],
        "share" => &["eve_brain::share"],
        "websearch" => &["eve_web::websearch"],
        "web_reader" => &["eve_web::web_reader"],
        "watch" => &["eve_web::watch"],
        "search_router" => &["eve_web::search_router"],
        "tinyfish" => &["eve_web::tinyfish"],
        "firecrawl" => &["eve_web::firecrawl"],
        "lights" => &["eve_lights"],
        "ears" => &["eve_ears"],
        "meet" => &["eve_meet"],
        "transcripts" => &["eve_transcripts"],
        "mem0_memory" => &["eve_mem0_memory"],
        "second_brain" => &["eve_second_brain"],
        "recall" => &["eve_records::recall"],
        "jev" => &["eve_jev"],
        "browser_router" => &["eve_browser_router"],
        "computer_agent" => &["eve_computer_agent"],
        "chief" => &["eve_chief"],
        "canvas" => &["eve_canvas"],
        "agentcard" => &["eve_agentcard"],
        "apps_mcp" => &["eve_apps_mcp"],
        "composio_tools" => &["eve_composio_tools"],
        _ => &[],
    }
}

/// Whether a record from `target` is one [`quiet`] drops.
pub(crate) fn is_quiet_target(target: &str) -> bool {
    QUIET.iter().flat_map(|n| quiet_targets(n).iter()).any(|p| {
        target == *p || (target.len() > p.len() && target.starts_with(p) && target[p.len()..].starts_with("::"))
    })
}

/// The thread's subscriber with the quiet modules' events taken out: everything else goes through unchanged.
struct QuietSubscriber {
    inner: tracing::Dispatch,
}

impl tracing::Subscriber for QuietSubscriber {
    fn register_callsite(&self, metadata: &'static tracing::Metadata<'static>) -> tracing::subscriber::Interest {
        // asked per record while this is the default, so the quiet test runs on every event
        let _ = self.inner.register_callsite(metadata);
        tracing::subscriber::Interest::sometimes()
    }

    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        if metadata.is_event() && is_quiet_target(metadata.target()) {
            return false;
        }
        self.inner.enabled(metadata)
    }

    fn new_span(&self, span: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        self.inner.new_span(span)
    }

    fn record(&self, span: &tracing::span::Id, values: &tracing::span::Record<'_>) {
        self.inner.record(span, values)
    }

    fn record_follows_from(&self, span: &tracing::span::Id, follows: &tracing::span::Id) {
        self.inner.record_follows_from(span, follows)
    }

    fn event(&self, event: &tracing::Event<'_>) {
        if is_quiet_target(event.metadata().target()) {
            return;
        }
        self.inner.event(event)
    }

    fn enter(&self, span: &tracing::span::Id) {
        self.inner.enter(span)
    }

    fn exit(&self, span: &tracing::span::Id) {
        self.inner.exit(span)
    }

    fn clone_span(&self, id: &tracing::span::Id) -> tracing::span::Id {
        self.inner.clone_span(id)
    }

    fn try_close(&self, id: tracing::span::Id) -> bool {
        self.inner.try_close(id)
    }
}

/// py: self_context.py:88 `def _quiet() -> Iterator[None]:`
///
/// The configured() calls below log their warnings a second time otherwise: drop them while the block is
/// built (a filter on those modules' own records, nothing else). Runs `f` with the filter on this thread.
pub fn quiet<R>(f: impl FnOnce() -> R) -> R {
    let inner = tracing::dispatcher::get_default(|d| d.clone());
    let quieted = tracing::Dispatch::new(QuietSubscriber { inner });
    tracing::dispatcher::with_default(&quieted, f)
}

// ---- the commit -------------------------------------------------------------------------------------------

static COMMIT: Mutex<Option<String>> = Mutex::new(None);

/// py: self_context.py:103 `def running_commit() -> str:`
///
/// The short sha of the checkout this package runs from, or "" (not a git checkout, no git). Read once per
/// process: a restart is what changes the running code, and a later checkout of another branch does not.
///
/// eve lite (EVE_PROFILE=lite) runs from a release with no checkout and starts no git (profile.py's firewall, the
/// developer-tools dialog a /usr/bin/git can open): "", with no process started. An EVE_PROFILE that is not a
/// profile is "" too, and is not remembered (Python's lru_cache keeps no result for a call that raised).
pub fn running_commit() -> String {
    try_running_commit().unwrap_or_default()
}

/// [`running_commit`], with the bad EVE_PROFILE Python raises as an error.
pub(crate) fn try_running_commit() -> anyhow::Result<String> {
    let mut cached = COMMIT.lock();
    if let Some(sha) = cached.as_ref() {
        return Ok(sha.clone());
    }
    let sha = commit_of(lite(None)?, &git_short_head);
    *cached = Some(sha.clone());
    Ok(sha)
}

/// `running_commit.cache_clear()`: the next call reads the commit again.
pub fn running_commit_cache_clear() {
    *COMMIT.lock() = None;
}

/// What `git` answered: (exit code 0, stdout), or None when it could not be started or timed out.
pub type GitRun = dyn Fn(&[String]) -> Option<(bool, String)>;

/// The commit line's sha, from `run` (the real one starts `git`): "" in eve lite with nothing started, else the
/// short sha when git answered with one.
pub fn commit_of(lite: bool, run: &GitRun) -> String {
    if lite {
        return String::new();
    }
    let here = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))
        .unwrap_or_else(|_| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
    let argv: Vec<String> =
        vec!["git".into(), "-C".into(), here.display().to_string(), "rev-parse".into(), "--short".into(), "HEAD".into()];
    let Some((ok, stdout)) = run(&argv) else {
        return String::new();
    };
    let sha = eve_envfile::py_strip(&stdout).to_string();
    if ok && !sha.is_empty() && sha.chars().all(char::is_alphanumeric) && sha.chars().count() <= 12 {
        sha
    } else {
        String::new()
    }
}

/// `subprocess.run(argv, capture_output=True, text=True, timeout=2.0)`: None on OSError or the timeout.
fn git_short_head(argv: &[String]) -> Option<(bool, String)> {
    let (program, args) = argv.split_first()?;
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            Ok(None) | Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let mut bytes = Vec::new();
        let _ = stdout.read_to_end(&mut bytes);
        out = String::from_utf8_lossy(&bytes).into_owned();
    }
    Some((status.success(), out))
}

/// py: self_context.py:120 `def _lite(environ: Optional[Mapping[str, str]] = None) -> bool:`
///
/// eve lite (profile.is_lite), for ``environ`` or the process's environment: profile.py is read when asked. An
/// EVE_PROFILE that is neither profile is the error Python raises (a ValueError).
pub(crate) fn lite(environ: Option<&Environ>) -> Result<bool, eve_profile::ProfileError> {
    eve_profile::is_lite(environ)
}

// ---- what the tests lend ------------------------------------------------------------------------------------

/// The search router as Python's tests patch it: what `available()` lists, and `SHIPPED`.
#[derive(Debug, Clone, PartialEq)]
pub struct Router {
    pub available: Vec<String>,
    pub shipped: bool,
}

/// What a caller (the tests) lends in place of the module state Python's tests patch.
#[derive(Debug, Clone, Default)]
pub struct Lent {
    /// the lights file (None: lights.py's default)
    pub lights_path: Option<PathBuf>,
    /// the sha (None: running_commit())
    pub commit: Option<String>,
    /// search_router.available and SHIPPED (None: the real router)
    pub router: Option<Router>,
    /// CONNECT_TOKEN_FILE, expanded (None: under this process's HOME)
    pub connect_file: Option<PathBuf>,
}

impl Lent {
    pub fn new(lights_path: Option<&Path>, commit: Option<&str>) -> Self {
        Lent {
            lights_path: lights_path.map(Path::to_path_buf),
            commit: commit.map(str::to_string),
            ..Lent::default()
        }
    }
}

// ---- one line per area ---------------------------------------------------------------------------------------

/// `ears.configured(env)`.
fn ears(env: &Environ) -> eve_ears::EarsConfig {
    eve_ears::configured_from(|k| env.get(k).cloned())
}

/// py: self_context.py:129 `def _models(env: Mapping[str, str]) -> list[str]:`
pub(crate) fn models(env: &Environ) -> Vec<String> {
    let key = eve_openrouter::available(Some(env));
    let mut out = Vec::new();
    let tg = crate::telegram::configured(Some(env));
    if tg.enabled {
        out.push(format!("Telegram brain (texts, calls, desk push-to-talk): {}, {} effort.", tg.model, tg.effort));
    }
    if ears(env).enabled && crate::voice_agent::live_available(Some(env)) {
        let vc = crate::voice_agent::configured(Some(env));
        out.push(format!(
            "Voice: {} talks; {} ({} effort) answers behind it.",
            vc.model, vc.backend_model, vc.backend_effort
        ));
    }
    let tc = crate::think::configured(Some(env), "");
    if tc.enabled && key {
        out.push(format!("think_hard: {}, {} effort.", tc.model, tc.effort));
    }
    let mc = crate::miniapp::configured(Some(env));
    if mc.enabled {
        out.push(format!("Mini App builder: {}.", mc.model));
    }
    out
}

/// py: self_context.py:149 `def _engine_words(engine: str) -> str:`
///
/// websearch.py's engine in words, from its own table: an OpenRouter engine by OpenRouter's name for it.
pub(crate) fn engine_words(engine: &str) -> String {
    if let Some(name) = websearch::openrouter_engine(engine) {
        return format!("{} through OpenRouter", capitalize(name));
    }
    if engine == "openai" {
        "OpenAI's hosted web search".to_string()
    } else {
        "off".to_string()
    }
}

/// py: self_context.py:158 `def _provider_words(provider: str, engine: str) -> str:`
///
/// One of search_router's providers in words. Its "perplexity" is websearch's engine search, so it is named by
/// the engine that actually runs; the others by their own name.
pub(crate) fn provider_words(provider: &str, engine: &str) -> String {
    if provider == "perplexity" {
        return engine_words(engine);
    }
    PROVIDER_WORDS.iter().find(|(k, _)| *k == provider).map(|(_, v)| v.to_string()).unwrap_or_else(|| provider.to_string())
}

/// py: self_context.py:166 `def _routed(env: Mapping[str, str], engine: str) -> tuple[str, ...]:`
///
/// The providers a routed search can really use here, or () when searches are not routed. Jev is shown every
/// option and the code masks the ones without a key (2026-09-29), so the option list is not the truth: this is
/// ``search_router.available`` (what this environment's keys and engine allow) under ``search_router.mode`` (off,
/// or auto before the router SHIPPED, routes nothing). One provider is the engine alone, not routing.
pub(crate) fn routed(env: &Environ, engine: &str, router: Option<&Router>) -> Vec<String> {
    let ctx = web_ctx(env);
    let mode = search_router::mode(&ctx);
    let found = match router {
        Some(r) => r.available.clone(),
        None => search_router::available(&ctx, engine),
    };
    let shipped = router.map_or(search_router::SHIPPED, |r| r.shipped);
    if mode == "off" || (mode == "auto" && !shipped) {
        return Vec::new();
    }
    let order: Vec<String> = search_router::PROVIDERS.iter().map(|p| p.to_string()).collect();
    let mut live: Vec<String> = order.iter().filter(|p| found.contains(p)).cloned().collect();
    live.extend(found.iter().filter(|p| !order.contains(p)).cloned());
    if live.len() > 1 {
        live
    } else {
        Vec::new()
    }
}

/// py: self_context.py:185 `def _search(env: Mapping[str, str]) -> str:`
///
/// The brain's web_search: routed per query by Jev across the providers usable on this environment
/// (``routed``, 2026-09-29), else the one engine.
pub(crate) fn search(env: &Environ, router: Option<&Router>) -> String {
    let cfg = websearch::configured(&web_ctx(env));
    let providers = routed(env, &cfg.engine, router);
    if !providers.is_empty() {
        let words: Vec<String> = providers.iter().map(|p| provider_words(p, &cfg.engine)).collect();
        return format!("Web search: Jev picks per query among {}.", join(&words));
    }
    if websearch::openrouter_engine(&cfg.engine).is_some() {
        return format!("Web search: {}; {} writes the answer.", engine_words(&cfg.engine), cfg.model);
    }
    format!("Web search: {}.", engine_words(&cfg.engine))
}

/// py: self_context.py:199 `def _computer(env: Mapping[str, str]) -> str:`
///
/// Tasks on the Mac: the agent (Codex, daemon._make_agent; its model is Codex's own setting), and what the web
/// reader (web_reader.py) tries first for a web reading task before handing it to that agent, one line for both
/// (they are one path).
pub(crate) fn computer(env: &Environ, router: Option<&Router>) -> String {
    // EVE_COMPUTER_CONTROL=0: no task body at all
    if !eve_computer_agent::configured(Some(env)).enabled {
        return "Tasks on the Mac: off.".to_string();
    }
    let floor = "Codex";
    let cfg = web_reader::configured(&web_ctx(env));
    if !cfg.enabled {
        return format!("Tasks on the Mac: {floor}.");
    }
    // routed like web search
    if !routed(env, &cfg.search.engine, router).is_empty() {
        return format!("Tasks on the Mac: {floor}; web reads try the routed search first.");
    }
    format!("Tasks on the Mac: {floor}; web reads try TinyFish ({} answers) first.", web_reader::MODEL)
}

/// py: self_context.py:215 `def _watch(env: Mapping[str, str]) -> str:`
pub(crate) fn watch(env: &Environ) -> String {
    let cfg = eve_web::watch::configured(&web_ctx(env));
    if !cfg.enabled {
        return "Watcher: off.".to_string();
    }
    let mut rungs = vec!["plain read".to_string()];
    if cfg.tls {
        rungs.push("Chrome-like retry".into());
    }
    if cfg.browser {
        rungs.push("headless Chromium".into());
    }
    if cfg.hosted {
        rungs.push("TinyFish".into());
    }
    // watch.py's own fallback
    let engine = websearch::openrouter_engine(&cfg.search.engine).unwrap_or("perplexity");
    rungs.push(format!("{} search", capitalize(engine)));
    format!("Watcher ladder: {}.", rungs.join(", "))
}

/// py: self_context.py:228 `def _lights(env: Mapping[str, str], path: Optional[Path]) -> str:`
pub(crate) fn lights(env: &Environ, path: Option<&Path>) -> String {
    if !eve_lights::enabled(Some(env)) {
        return "Lights: off.".to_string();
    }
    let kinds: HashSet<String> = eve_lights::load(path).into_iter().map(|lt| lt.kind).collect();
    let brands: Vec<String> = eve_lights::KINDS
        .iter()
        .filter(|k| kinds.contains(**k))
        .map(|k| LIGHT_BRANDS.iter().find(|(b, _)| b == k).map_or(k.to_string(), |(_, w)| w.to_string()))
        .collect();
    if brands.is_empty() {
        "Lights: none set up.".to_string()
    } else {
        format!("Lights: {}.", join(&brands))
    }
}

/// py: self_context.py:238 `def _memory(env: Mapping[str, str]) -> str:`
pub(crate) fn memory(env: &Environ) -> String {
    let mut stores: Vec<String> = Vec::new();
    if eve_transcripts::configured(Some(env)).enabled {
        stores.push("day transcripts".into());
        stores.push("records and profile".into());
        if eve_mem0_memory::configured(&eve_records::configured(Some(env)), Some(env)).enabled
            && eve_mem0_memory::available()
        {
            stores.push("mem0 index (OpenAI)".into());
        }
    }
    if eve_second_brain::configured(Some(env)).enabled {
        stores.push("second brain vault".into());
    }
    if stores.is_empty() {
        "Memory: off.".to_string()
    } else {
        format!("Memory: {}.", join(&stores))
    }
}

/// py: self_context.py:251 `def _doors(env: Mapping[str, str]) -> str:`
pub(crate) fn doors(env: &Environ) -> String {
    let tg = crate::telegram::configured(Some(env)).enabled;
    let mut doors: Vec<String> = Vec::new();
    if tg {
        doors.push("Telegram".into());
    }
    if crate::miniapp::configured(Some(env)).enabled {
        doors.push("the Mini App".into());
    }
    if tg && eve_meet::configured(Some(env)).enabled {
        doors.push("the Meet notetaker".into());
    }
    if ears(env).enabled && crate::voice_agent::live_available(Some(env)) {
        doors.push("voice (wake word)".into());
    }
    if doors.is_empty() {
        "Doors on: none.".to_string()
    } else {
        format!("Doors on: {}.", join(&doors))
    }
}

/// py: self_context.py:261 `def _canvas(env: Mapping[str, str]) -> list[str]:`
///
/// One line while Canvas is set up (canvas.configured: CANVAS_BASE_URL and CANVAS_API_TOKEN), else none, so a
/// eve without it has the block it had. The school's address is not named: the line says what, not where.
pub(crate) fn canvas(env: &Environ) -> Vec<String> {
    let on = eve_canvas::configured(Some(env)).enabled && crate::telegram::configured(Some(env)).enabled;
    if on {
        vec!["Canvas (school courses): on, read only.".to_string()]
    } else {
        Vec::new()
    }
}

/// py: self_context.py:270 `def _chief(env: Mapping[str, str]) -> list[str]:`
///
/// One line while the chief of staff is on, else none (chief.enabled reads the same switch the daemon builds
/// it from).
pub(crate) fn chief(env: &Environ) -> Vec<String> {
    if eve_chief::enabled(Some(env)) {
        vec!["Chief of staff: on.".to_string()]
    } else {
        Vec::new()
    }
}

/// py: self_context.py:278 `def _agentcard(env: Mapping[str, str]) -> list[str]:`
///
/// Only capability state, with no token, address, cart or purchase history in the stable prompt.
pub(crate) fn agentcard(env: &Environ) -> Vec<String> {
    // agentcard.configured's ValueError reads as off
    let on = crate::telegram::configured(Some(env)).enabled
        && eve_openrouter::available(Some(env))
        && eve_agentcard::configured(Some(env)).map(|c| c.enabled).unwrap_or(false);
    if on {
        vec!["AgentCard: chat/call purchases; exact cart needs owner approval.".to_string()]
    } else {
        Vec::new()
    }
}

/// py: self_context.py:291 `_APP_RULES`
/// apps_mcp's policies in words, grouped so one line carries every app (each app's name is apps_mcp.SERVERS' label)
pub const APP_RULES: &[(&str, &str)] = &[("read", "read only"), ("auto", "routine changes run, sends and deletes asked first"), ("write", "may change"), ("ask", "changes asked first")];

/// py: self_context.py:295 `def _apps(env: Mapping[str, str]) -> list[str]:`
///
/// The owner's apps, one line per kind, only while the Telegram brain (their only door) is on: the direct ones
/// that are signed in, with their write rule; Composio's toolkits while it has a key and an owner.
pub(crate) fn apps(env: &Environ) -> Vec<String> {
    let tg = crate::telegram::configured(Some(env));
    if !tg.enabled {
        return Vec::new();
    }
    let mut out = Vec::new();
    let cfg = eve_apps_mcp::configured(Some(env));
    let up: Vec<String> =
        if cfg.enabled { eve_apps_mcp::AppsBridge::new(cfg.clone()).available() } else { Vec::new() };
    if !up.is_empty() {
        let rule_of = |a: &str| cfg.policy.get(a).map(String::as_str).unwrap_or("ask").to_string();
        let mut rules = Vec::new();
        for (rule, words) in APP_RULES {
            let named: Vec<String> = up
                .iter()
                .filter(|a| rule_of(a) == *rule)
                .map(|a| eve_apps_mcp::SERVERS.get(a.as_str()).map_or(a.clone(), |s| s.label.to_string()))
                .collect();
            if !named.is_empty() {
                rules.push(format!("{} {words}", join(&named)));
            }
        }
        out.push(format!("Own apps: {}.", rules.join("; ")));
    }
    let owners: BTreeSet<i64> = tg.owner_ids.iter().copied().collect();
    let cc = eve_composio_tools::configured(Some(env), &owners);
    if cc.enabled {
        let kits: Vec<String> = cc.toolkits.clone().unwrap_or_default();
        if kits.is_empty() {
            out.push("Composio apps: all.".to_string());
        } else {
            let words: Vec<String> = kits.iter().map(|k| capitalize(k)).collect();
            out.push(format!("Composio apps: {}.", join(&words)));
        }
    }
    out
}

/// `sys.platform`: "darwin" on a Mac, else the OS's own name ("linux", ...).
fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "darwin"
    } else {
        std::env::consts::OS
    }
}

/// py: self_context.py:317 `def _sharing(env: Mapping[str, str]) -> list[str]:`
///
/// File sharing, one line while on (with Telegram, its door).
pub(crate) fn sharing(env: &Environ) -> Vec<String> {
    if !crate::telegram::configured(Some(env)).enabled {
        return Vec::new();
    }
    let mut out = Vec::new();
    let sc = crate::share::configured(Some(env), platform());
    if sc.enabled {
        let email = if crate::share::gmail_can_send(Some(env)) {
            "email, attached from Gmail (over 18 MB, a Drive link)"
        } else {
            "email as a Drive link"
        };
        let mut vias: Vec<String> = Vec::new();
        if sc.imessage {
            vias.push("iMessage".into());
        }
        if crate::share::google_token(Some(env)).is_some() {
            vias.push(email.into());
        }
        if !vias.is_empty() {
            out.push(format!("File sharing: {}, on the owner's Allow.", join(&vias)));
        }
    }
    out
}

/// `os.environ if environ is None else environ`: the process's environment, read now, as a mapping.
fn env_of(environ: Option<&Environ>) -> Environ {
    match environ {
        Some(env) => env.clone(),
        None => std::env::vars_os()
            .filter_map(|(k, v)| Some((k.into_string().ok()?, v.into_string().ok()?)))
            .collect(),
    }
}

/// py: self_context.py:334 `def lines(environ: Optional[Mapping[str, str]] = None, *, lights_path: Optional[Path] = None, commit: Optional[str] = None) -> list[str]:`
///
/// The block's lines, in order. `lights_path` and `commit` are lent by tests (the lights file and the sha). An
/// error is what Python raises (an EVE_PROFILE that is neither profile).
pub fn lines(environ: Option<&Environ>, lights_path: Option<&Path>, commit: Option<&str>) -> anyhow::Result<Vec<String>> {
    lines_lent(environ, &Lent::new(lights_path, commit))
}

/// [`lines`] with everything a test lends.
pub fn lines_lent(environ: Option<&Environ>, lent: &Lent) -> anyhow::Result<Vec<String>> {
    let env = env_of(environ);
    if lite(Some(&env))? {
        return Ok(lite_lines_lent(&env, lent.commit.as_deref(), lent));
    }
    let sha = match &lent.commit {
        Some(c) => c.clone(),
        None => try_running_commit()?,
    };
    let router = lent.router.as_ref();
    let mut out = quiet(|| {
        let mut out = models(&env);
        out.push(search(&env, router));
        out.push(computer(&env, router));
        out.push(watch(&env));
        out.push(lights(&env, lent.lights_path.as_deref()));
        out.push(memory(&env));
        out.push(doors(&env));
        out.extend(canvas(&env));
        out.extend(apps(&env));
        out.extend(sharing(&env));
        out.extend(chief(&env));
        out.extend(agentcard(&env));
        out
    });
    if !sha.is_empty() {
        out.push(format!("Running code: git commit {sha}."));
    }
    Ok(out)
}

// ---- eve lite (EVE_PROFILE=lite, profile.py) -----------------------------------------------------------

/// py: self_context.py:352 `def _lite_lines(env: Mapping[str, str], commit: Optional[str]) -> list[str]:`
///
/// eve lite's block: only what eve lite is made of (lite.py's LiteDaemon), each line read from the same
/// configured() as full eve's. Its one door is eve's web app; its apps are the owner's Composio Connect sign-in. The
/// Mac's tasks, the lights, the voice's halves, the Mini App, Canvas, sharing, the chief and purchases are not part
/// of it at all, so no line names them, not even as off. The commit is "" with no git started (running_commit);
/// a test may still lend one.
#[allow(dead_code)] // the module function as Python names it; the block reads it through what the tests lend
pub(crate) fn lite_lines(env: &Environ, commit: Option<&str>) -> Vec<String> {
    lite_lines_lent(env, commit, &Lent::default())
}

fn lite_lines_lent(env: &Environ, commit: Option<&str>, lent: &Lent) -> Vec<String> {
    let router = lent.router.as_ref();
    let mut out = quiet(|| {
        let mut out = lite_models(env);
        out.push(search(env, router));
        out.push(watch(env));
        out.push(memory(env));
        out.push(LITE_DOORS.to_string());
        out.extend(lite_apps_at(lent.connect_file.as_deref()));
        out
    });
    if let Some(c) = commit.filter(|c| !c.is_empty()) {
        out.push(format!("Running code: git commit {c}."));
    }
    out
}

/// py: self_context.py:366 `LITE_DOORS`
/// eve lite's one door: the web app, in a browser on the Mac or on a phone or laptop on the same Wi-Fi (lite_web.py).
pub const LITE_DOORS: &str = "Doors on: eve's web app.";

/// py: self_context.py:367 `LITE_APPS`
pub const LITE_APPS: &str = "Apps: the owner's apps connected in Composio, through the COMPOSIO tools.";

/// py: self_context.py:370 `def _lite_models(env: Mapping[str, str]) -> list[str]:`
///
/// The chat brain (telegram.py's, the web app's only brain, for texts and push-to-talk alike) and think_hard.
pub(crate) fn lite_models(env: &Environ) -> Vec<String> {
    let tg = crate::telegram::configured(Some(env));
    let mut out = vec![format!("Chat brain (eve's web app: texts and push-to-talk): {}, {} effort.", tg.model, tg.effort)];
    let tc = crate::think::configured(Some(env), "");
    if tc.enabled && eve_openrouter::available(Some(env)) {
        out.push(format!("think_hard: {}, {} effort.", tc.model, tc.effort));
    }
    out
}

/// py: self_context.py:382 `def _lite_apps() -> list[str]:`
///
/// One line while the owner is signed in to Composio Connect (onboarding writes CONNECT_TOKEN_FILE only once a
/// token arrives, so the file is the sign-in), else none: its apps are whichever the owner connected there, so none
/// is named. Its presence is all that is read here, never its tokens.
#[allow(dead_code)] // the module function as Python names it; the block reads it through what the tests lend
pub(crate) fn lite_apps() -> Vec<String> {
    lite_apps_at(None)
}

fn lite_apps_at(path: Option<&Path>) -> Vec<String> {
    let file = match path {
        Some(p) => p.to_path_buf(),
        None => expanduser(crate::onboarding::CONNECT_TOKEN_FILE),
    };
    if file.is_file() {
        vec![LITE_APPS.to_string()]
    } else {
        Vec::new()
    }
}

/// Sets `BUILDING` back when the block is done, however it ends.
struct Building;

impl Drop for Building {
    fn drop(&mut self) {
        BUILDING.with(|b| b.set(false));
    }
}

/// py: self_context.py:395 `def block(environ: Optional[Mapping[str, str]] = None, *, lights_path: Optional[Path] = None, commit: Optional[str] = None) -> str:`
///
/// The block for a prompt: the header, "- " lines, the rule. "" when it cannot be built (it is context, and a
/// door never fails over it) or while another configured() is building one (the doors read each other).
pub fn block(environ: Option<&Environ>, lights_path: Option<&Path>, commit: Option<&str>) -> String {
    block_lent(environ, &Lent::new(lights_path, commit))
}

/// [`block`] with everything a test lends.
pub fn block_lent(environ: Option<&Environ>, lent: &Lent) -> String {
    if BUILDING.with(Cell::get) {
        return String::new();
    }
    let body = {
        BUILDING.with(|b| b.set(true));
        let _building = Building;
        // Python catches every Exception here: a missing optional part must never stop a door from opening. A
        // part that panics is caught the same way.
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| lines_lent(environ, lent))) {
            Ok(Ok(body)) => body,
            Ok(Err(e)) => {
                let kind = if e.downcast_ref::<eve_profile::ProfileError>().is_some() { "ValueError" } else { "Error" };
                tracing::warn!("self context: not built ({}: {})", kind, e);
                return String::new();
            }
            Err(panic) => {
                let why = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_default();
                tracing::warn!("self context: not built (panic: {})", why);
                return String::new();
            }
        }
    };
    let listed: Vec<String> = body.iter().map(|ln| format!("- {ln}")).collect();
    let text = format!("{HEADER}\n{}\n{RULE}", listed.join("\n"));
    let n = text.chars().count();
    if n > BUDGET {
        tracing::info!("self context: {} chars, over the {} budget", n, BUDGET);
    }
    text
}
