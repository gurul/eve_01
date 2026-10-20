//! A port of `self_context.py`, the whole module.
//!
//! Skeleton: constants and prompts are verbatim (generated from the Python values); bodies are
//! `todo!()` until this file's unit fills them.

#[allow(unused_imports)]
use crate::prelude::*;

// py: self_context.py:58 `log` (Logger): ported by this file's unit.

/// py: self_context.py:60 `BUDGET`
/// characters for the whole block: every turn of every door pays for it
pub const BUDGET: usize = 1450;

/// py: self_context.py:64 `HEADER`
pub const HEADER: &str = "

Your live setup, from eve's code and settings at start:";

/// py: self_context.py:67 `RULE`
pub const RULE: &str = "This list is the source of truth for the models, services and providers you run on, over memory and past chats: if it does not name one, say you do not know; never guess. What you can do is your tools.";

/// py: self_context.py:70 `LIGHT_BRANDS`
pub const LIGHT_BRANDS: &[(&str, &str)] = &[("govee", "Govee"), ("wiz", "WiZ"), ("triones", "HappyLighting"), ("tuya", "Sylvania (Tuya)")];

/// py: self_context.py:73 `PROVIDER_WORDS`
pub const PROVIDER_WORDS: &[(&str, &str)] = &[("tinyfish", "TinyFish"), ("firecrawl", "Firecrawl")];

/// py: self_context.py:75 `_QUIET`
pub(crate) const QUIET: &[&str] = &["telegram", "voice_agent", "think", "miniapp", "websearch", "web_reader", "watch", "lights", "ears", "meet", "transcripts", "mem0_memory", "second_brain", "recall", "search_router", "jev", "tinyfish", "firecrawl", "browser_router", "computer_agent", "chief", "canvas", "agentcard", "apps_mcp", "composio_tools", "share"];

// py: self_context.py:80 `_building` (_local): ported by this file's unit.

/// py: self_context.py:83 `def _join(words: list[str]) -> str:`
pub(crate) fn join(words: &[String]) -> String {
    todo!("self_context.py:83 _join")
}

/// py: self_context.py:88 `def _quiet() -> Iterator[None]:`
///
/// The configured() calls below log their warnings a second time otherwise: drop them while the block is
/// built (a filter on those modules' own loggers, nothing else).
pub(crate) fn quiet() -> Vec<()> {
    todo!("self_context.py:88 _quiet")
}

/// py: self_context.py:103 `def running_commit() -> str:`
///
/// The short sha of the checkout this package runs from, or "" (not a git checkout, no git). Read once per
/// process: a restart is what changes the running code, and a later checkout of another branch does not.
///
/// eve lite (EVE_PROFILE=lite) runs from a release with no checkout and starts no git (profile.py's firewall, the
/// developer-tools dialog a /usr/bin/git can open): "", with no process started.
pub fn running_commit() -> String {
    todo!("self_context.py:103 running_commit")
}

/// py: self_context.py:120 `def _lite(environ: Optional[Mapping[str, str]] = None) -> bool:`
///
/// eve lite (profile.is_lite), for ``environ`` or the process's environment: profile.py is read when asked.
pub(crate) fn lite(environ: Option<&Environ>) -> bool {
    todo!("self_context.py:120 _lite")
}

/// py: self_context.py:129 `def _models(env: Mapping[str, str]) -> list[str]:`
pub(crate) fn models(env: &Environ) -> Vec<String> {
    todo!("self_context.py:129 _models")
}

/// py: self_context.py:149 `def _engine_words(engine: str) -> str:`
///
/// websearch.py's engine in words, from its own table: an OpenRouter engine by OpenRouter's name for it.
pub(crate) fn engine_words(engine: &str) -> String {
    todo!("self_context.py:149 _engine_words")
}

/// py: self_context.py:158 `def _provider_words(provider: str, engine: str) -> str:`
///
/// One of search_router's providers in words. Its "perplexity" is websearch's engine search, so it is named by
/// the engine that actually runs; the others by their own name.
pub(crate) fn provider_words(provider: &str, engine: &str) -> String {
    todo!("self_context.py:158 _provider_words")
}

/// py: self_context.py:166 `def _routed(env: Mapping[str, str], engine: str) -> tuple[str, ...]:`
///
/// The providers a routed search can really use here, or () when searches are not routed. Jev is shown every
/// option and the code masks the ones without a key (2026-09-29), so the option list is not the truth: this is
/// ``search_router.available`` (what this environment's keys and engine allow) under ``search_router.mode`` (off,
/// or auto before the router SHIPPED, routes nothing). One provider is the engine alone, not routing. A tree
/// without the router, or a router without these, is the engine alone too.
pub(crate) fn routed(env: &Environ, engine: &str) -> Vec<String> {
    todo!("self_context.py:166 _routed")
}

/// py: self_context.py:185 `def _search(env: Mapping[str, str]) -> str:`
///
/// The brain's web_search: routed per query by Jev across the providers usable on this environment
/// (``_routed``, 2026-09-29), else the one engine.
pub(crate) fn search(env: &Environ) -> String {
    todo!("self_context.py:185 _search")
}

/// py: self_context.py:199 `def _computer(env: Mapping[str, str]) -> str:`
///
/// Tasks on the Mac: the agent (Codex, daemon._make_agent; its model is Codex's own setting), and what the web reader (web_reader.py) tries first for a web
/// reading task before handing it to that agent, one line for both (they are one path).
pub(crate) fn computer(env: &Environ) -> String {
    todo!("self_context.py:199 _computer")
}

/// py: self_context.py:215 `def _watch(env: Mapping[str, str]) -> str:`
pub(crate) fn watch(env: &Environ) -> String {
    todo!("self_context.py:215 _watch")
}

/// py: self_context.py:228 `def _lights(env: Mapping[str, str], path: Optional[Path]) -> str:`
pub(crate) fn lights(env: &Environ, path: Option<&Path>) -> String {
    todo!("self_context.py:228 _lights")
}

/// py: self_context.py:238 `def _memory(env: Mapping[str, str]) -> str:`
pub(crate) fn memory(env: &Environ) -> String {
    todo!("self_context.py:238 _memory")
}

/// py: self_context.py:251 `def _doors(env: Mapping[str, str]) -> str:`
pub(crate) fn doors(env: &Environ) -> String {
    todo!("self_context.py:251 _doors")
}

/// py: self_context.py:261 `def _canvas(env: Mapping[str, str]) -> list[str]:`
///
/// One line while Canvas is set up (canvas.configured: CANVAS_BASE_URL and CANVAS_API_TOKEN), else none, so a
/// eve without it has the block it had. The school's address is not named: the line says what, not where.
pub(crate) fn canvas(env: &Environ) -> Vec<String> {
    todo!("self_context.py:261 _canvas")
}

/// py: self_context.py:270 `def _chief(env: Mapping[str, str]) -> list[str]:`
///
/// One line while the chief of staff is on, else none (chief.enabled reads the same switch the daemon builds
/// it from).
pub(crate) fn chief(env: &Environ) -> Vec<String> {
    todo!("self_context.py:270 _chief")
}

/// py: self_context.py:278 `def _agentcard(env: Mapping[str, str]) -> list[str]:`
///
/// Only capability state, with no token, address, cart or purchase history in the stable prompt.
pub(crate) fn agentcard(env: &Environ) -> Vec<String> {
    todo!("self_context.py:278 _agentcard")
}

/// py: self_context.py:291 `_APP_RULES`
pub(crate) const APP_RULES: &[(&str, &str)] = &[("read", "read only"), ("auto", "routine changes run, sends and deletes asked first"), ("write", "may change"), ("ask", "changes asked first")];

/// py: self_context.py:295 `def _apps(env: Mapping[str, str]) -> list[str]:`
///
/// The owner's apps, one line per kind, only while the Telegram brain (their only door) is on: the direct ones
/// that are signed in, with their write rule; Composio's toolkits while it has a key and an owner.
pub(crate) fn apps(env: &Environ) -> Vec<String> {
    todo!("self_context.py:295 _apps")
}

/// py: self_context.py:317 `def _sharing(env: Mapping[str, str]) -> list[str]:`
///
/// File sharing, one line while on (with Telegram, its door).
pub(crate) fn sharing(env: &Environ) -> Vec<String> {
    todo!("self_context.py:317 _sharing")
}

/// py: self_context.py:334 `def lines(environ: Optional[Mapping[str, str]] = None, *, lights_path: Optional[Path] = None, commit: Optional[str] = None) -> list[str]:`
///
/// The block's lines, in order. `lights_path` and `commit` are lent by tests (the lights file and the sha).
pub fn lines(environ: Option<&Environ>, lights_path: Option<&Path>, commit: Option<&str>) -> Vec<String> {
    todo!("self_context.py:334 lines")
}

/// py: self_context.py:352 `def _lite_lines(env: Mapping[str, str], commit: Optional[str]) -> list[str]:`
///
/// eve lite's block: only what eve lite is made of (lite.py's LiteDaemon), each line read from the same
/// configured() as full eve's. Its one door is eve's web app; its apps are the owner's Composio Connect sign-in. The
/// Mac's tasks, the lights, the voice's halves, the Mini App, Canvas, sharing, the chief and purchases are not part
/// of it at all, so no line names them, not even as off. The commit is "" with no git started (running_commit);
/// a test may still lend one.
pub(crate) fn lite_lines(env: &Environ, commit: Option<&str>) -> Vec<String> {
    todo!("self_context.py:352 _lite_lines")
}

/// py: self_context.py:366 `LITE_DOORS`
pub const LITE_DOORS: &str = "Doors on: eve's web app.";

/// py: self_context.py:367 `LITE_APPS`
pub const LITE_APPS: &str = "Apps: the owner's apps connected in Composio, through the COMPOSIO tools.";

/// py: self_context.py:370 `def _lite_models(env: Mapping[str, str]) -> list[str]:`
///
/// The chat brain (telegram.py's, the web app's only brain, for texts and push-to-talk alike) and think_hard.
pub(crate) fn lite_models(env: &Environ) -> Vec<String> {
    todo!("self_context.py:370 _lite_models")
}

/// py: self_context.py:382 `def _lite_apps() -> list[str]:`
///
/// One line while the owner is signed in to Composio Connect (onboarding writes CONNECT_TOKEN_FILE only once a
/// token arrives, so the file is the sign-in), else none: its apps are whichever the owner connected there, so none
/// is named. Its presence is all that is read here, never its tokens.
pub(crate) fn lite_apps() -> Vec<String> {
    todo!("self_context.py:382 _lite_apps")
}

/// py: self_context.py:395 `def block(environ: Optional[Mapping[str, str]] = None, *, lights_path: Optional[Path] = None, commit: Optional[str] = None) -> str:`
///
/// The block for a prompt: the header, "- " lines, the rule. "" when it cannot be built (it is context, and a
/// door never fails over it) or while another configured() is building one (the doors read each other).
pub fn block(environ: Option<&Environ>, lights_path: Option<&Path>, commit: Option<&str>) -> String {
    todo!("self_context.py:395 block")
}
