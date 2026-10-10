"""Self-context: what eve runs on right now, read from its own code and settings, for its brains to read.

The owner texted "what do you use for search" on 2026-09-29 and eve answered from a memory record (a
``memory_search`` hit) naming an engine it had not used since 2026-09-24. Nothing in any prompt described eve's
own wiring, so a question about it was answered from memory, and memory drifts: a record says what was true the
day it was written. This module writes a short block, one line per area, from the same ``configured()`` calls the
daemon builds its doors from, so it cannot drift from what runs:

* the models per door: the Telegram brain (telegram.py), the voice's two halves (voice_agent.py), think_hard
  (think.py) and the Mini App's builder (miniapp.py);
* web search: the providers that are usable on this environment when the search router routes
  (``search_router.available`` and ``mode``, 2026-09-29; imported defensively, so a tree without the router still
  builds the block), else websearch.py's one engine. Only live providers are named, never the full option set Jev
  is shown;
* tasks on the Mac: the agent the daemon builds for one (daemon._make_agent: Codex; computer_agent.configured
  turns it off), and what a web reading task tries first (web_reader.py);
* the watcher's reading ladder (watch.py), in the order the watcher climbs it;
* the lights by brand (lights.py's file, as the daemon loads it at boot), the memory stores, the doors that are
  on, and the git commit the code runs from;
* Canvas (canvas.py), one short line and only while it is set up (2026-09-30), so the owner's "can you see my
  Canvas" is answered from what runs;
* the owner's apps, one short line each and only while on (2026-10-04): the direct apps that are signed in
  (apps_mcp.AppsBridge.available: files and env only, no network) with what may change in each, Composio's
  toolkits and file sharing (share.py), so "can you share a file" or "which apps do you have" is answered from
  what runs;
* the chief of staff (chief.py), one short line and only while it is on (``chief.enabled``: EVE_CHIEF=on, or
  auto once its capture eval has passed). Off, the block is byte for byte what it was (P4, 2026-09-29). What the
  chief is doing lives in each turn's note (chief.for_turn), never here: this block is fixed from boot.

No value is typed here: every model, engine and brand comes from the module that uses it, and a component that
is off says so. The only fixed words are how each thing is described. Names are only ever the ones that are live
now: never a retired one, not even as "not X" (the owner's rule for prompts: a name in a prompt is a sample of
what the model says).

Each door's config carries the block (``about``), filled once in its ``configured()``: the daemon builds those at
boot from the same environment and the same lights file its live objects come from, so the block describes the
running daemon, and it is byte-identical from turn to turn, which keeps it inside the prompt cache's stable
prefix. A config built by hand (the tests) has ``about=""`` and the prompt is what it was before. Target: under
BUDGET characters, the rule sentence included (measured 2026-09-29 on the owner's environment: see the docs).
"""

from __future__ import annotations

import contextlib
import functools
import logging
import os
import subprocess
import threading
from pathlib import Path
from typing import Iterator, Mapping, Optional

log = logging.getLogger(__name__)

BUDGET = 1450                         # characters for the whole block: every turn of every door pays for it
# (1000 until 2026-09-29, when the line for tasks on the Mac came in: everything on, routed, was 1022; 1100 until
# 2026-10-04, when the owner's apps, Composio's apps, an ordering line (removed 2026-10-05) and file sharing came
# in: everything on, GitHub signed in too, was 1372)
HEADER = "\n\nYour live setup, from eve's code and settings at start:"
# Scoped to what the list is (owner, 2026-09-29): the source of truth for the models, services and providers eve
# runs on. What eve can do is its tool list, so a question about a tool is never answered "I do not know" here.
RULE = ("This list is the source of truth for the models, services and providers you run on, over memory and past "
        "chats: if it does not name one, say you do not know; never guess. What you can do is your tools.")
# How each brand of light is called by the owner (lights.KINDS is the set; "triones" is HappyLighting's protocol).
LIGHT_BRANDS = {"govee": "Govee", "wiz": "WiZ", "triones": "HappyLighting", "tuya": "Sylvania (Tuya)"}
# search_router.PROVIDERS in words ("perplexity" is named by websearch's engine, _provider_words). A name not here
# is shown as the router gives it.
PROVIDER_WORDS = {"tinyfish": "TinyFish", "firecrawl": "Firecrawl"}
# The modules whose configured() this reads: their warnings were already logged once when the daemon built them.
_QUIET = ("telegram", "voice_agent", "think", "miniapp", "websearch", "web_reader", "watch", "lights", "ears",
          "meet", "transcripts", "mem0_memory", "second_brain", "recall", "search_router", "jev", "tinyfish",
          "firecrawl", "browser_router", "computer_agent", "chief", "canvas", "agentcard", "apps_mcp",
          "composio_tools", "share")

_building = threading.local()          # a configured() that builds the block reads other configured()s: no loop


def _join(words: list[str]) -> str:
    return words[0] if len(words) == 1 else ", ".join(words[:-1]) + " and " + words[-1]


@contextlib.contextmanager
def _quiet() -> Iterator[None]:
    """The configured() calls below log their warnings a second time otherwise: drop them while the block is
    built (a filter on those modules' own loggers, nothing else)."""
    loggers = [logging.getLogger(f"{__package__}.{name}") for name in _QUIET]
    drop = logging.Filter(name="\0")    # a filter whose name matches no logger: every record is dropped
    for lg in loggers:
        lg.addFilter(drop)
    try:
        yield
    finally:
        for lg in loggers:
            lg.removeFilter(drop)


@functools.lru_cache(maxsize=1)
def running_commit() -> str:
    """The short sha of the checkout this package runs from, or "" (not a git checkout, no git). Read once per
    process: a restart is what changes the running code, and a later checkout of another branch does not."""
    try:
        out = subprocess.run(["git", "-C", str(Path(__file__).resolve().parent), "rev-parse", "--short", "HEAD"],
                             capture_output=True, text=True, timeout=2.0, check=False)
    except (OSError, subprocess.SubprocessError):
        return ""
    sha = out.stdout.strip()
    return sha if out.returncode == 0 and sha.isalnum() and len(sha) <= 12 else ""


# ---- one line per area --------------------------------------------------------------------------

def _models(env: Mapping[str, str]) -> list[str]:
    from . import ears, miniapp, openrouter, telegram, think, voice_agent

    key = openrouter.available(env)
    out = []
    tg = telegram.configured(env)
    if tg.enabled:
        out.append(f"Telegram brain (texts, calls, desk push-to-talk): {tg.model}, {tg.effort} effort.")
    if ears.configured(env).enabled and voice_agent.live_available(env):
        vc = voice_agent.configured(env)
        out.append(f"Voice: {vc.model} talks; {vc.backend_model} ({vc.backend_effort} effort) answers behind it.")
    tc = think.configured(env)
    if tc.enabled and key:
        out.append(f"think_hard: {tc.model}, {tc.effort} effort.")
    mc = miniapp.configured(env)
    if mc.enabled:
        out.append(f"Mini App builder: {mc.model}.")
    return out


def _engine_words(engine: str) -> str:
    """websearch.py's engine in words, from its own table: an OpenRouter engine by OpenRouter's name for it."""
    from . import websearch

    if engine in websearch.OPENROUTER_ENGINES:
        return f"{websearch.OPENROUTER_ENGINES[engine].capitalize()} through OpenRouter"
    return "OpenAI's hosted web search" if engine == "openai" else "off"


def _provider_words(provider: str, engine: str) -> str:
    """One of search_router's providers in words. Its "perplexity" is websearch's engine search, so it is named by
    the engine that actually runs; the others by their own name."""
    if provider == "perplexity":
        return _engine_words(engine)
    return PROVIDER_WORDS.get(provider, provider)


def _routed(env: Mapping[str, str], engine: str) -> tuple[str, ...]:
    """The providers a routed search can really use here, or () when searches are not routed. Jev is shown every
    option and the code masks the ones without a key (2026-09-29), so the option list is not the truth: this is
    ``search_router.available`` (what this environment's keys and engine allow) under ``search_router.mode`` (off,
    or auto before the router SHIPPED, routes nothing). One provider is the engine alone, not routing. A tree
    without the router, or a router without these, is the engine alone too."""
    try:
        from . import search_router
        mode = search_router.mode(env)
        found = tuple(search_router.available(env, engine) or ())
    except (ImportError, AttributeError, TypeError):
        return ()
    if mode == "off" or (mode == "auto" and not getattr(search_router, "SHIPPED", False)):
        return ()
    order = tuple(getattr(search_router, "PROVIDERS", found))
    live = tuple(p for p in order if p in found) + tuple(p for p in found if p not in order)
    return live if len(live) > 1 else ()


def _search(env: Mapping[str, str]) -> str:
    """The brain's web_search: routed per query by Jev across the providers usable on this environment
    (``_routed``, 2026-09-29), else the one engine."""
    from . import websearch

    cfg = websearch.configured(env)
    providers = _routed(env, cfg.engine)
    if providers:
        return f"Web search: Jev picks per query among {_join([_provider_words(p, cfg.engine) for p in providers])}."
    if cfg.engine in websearch.OPENROUTER_ENGINES:
        return f"Web search: {_engine_words(cfg.engine)}; {cfg.model} writes the answer."
    return f"Web search: {_engine_words(cfg.engine)}."


def _computer(env: Mapping[str, str]) -> str:
    """Tasks on the Mac: the agent (Codex, daemon._make_agent; its model is Codex's own setting), and what the web reader (web_reader.py) tries first for a web
    reading task before handing it to that agent, one line for both (they are one path)."""
    from . import computer_agent, web_reader

    if not computer_agent.configured(env).enabled:       # EVE_COMPUTER_CONTROL=0: no task body at all
        return "Tasks on the Mac: off."
    floor = "Codex"
    cfg = web_reader.configured(env)
    if not cfg.enabled:
        return f"Tasks on the Mac: {floor}."
    if _routed(env, getattr(getattr(cfg, "search", None), "engine", "")):   # routed like web search
        return f"Tasks on the Mac: {floor}; web reads try the routed search first."
    return f"Tasks on the Mac: {floor}; web reads try TinyFish ({web_reader.MODEL} answers) first."


def _watch(env: Mapping[str, str]) -> str:
    from . import watch, websearch

    cfg = watch.configured(env)
    if not cfg.enabled:
        return "Watcher: off."
    rungs = ["plain read"] + (["Chrome-like retry"] if cfg.tls else []) + (
        ["headless Chromium"] if cfg.browser else []) + (["TinyFish"] if cfg.hosted else [])
    engine = websearch.OPENROUTER_ENGINES.get(cfg.search.engine, "perplexity")   # watch.py's own fallback
    rungs.append(f"{engine.capitalize()} search")
    return f"Watcher ladder: {', '.join(rungs)}."


def _lights(env: Mapping[str, str], path: Optional[Path]) -> str:
    from . import lights

    if not lights.enabled(env):
        return "Lights: off."
    kinds = {lt.kind for lt in lights.load(path)}
    brands = [LIGHT_BRANDS.get(k, k) for k in lights.KINDS if k in kinds]
    return f"Lights: {_join(brands)}." if brands else "Lights: none set up."


def _memory(env: Mapping[str, str]) -> str:
    from . import mem0_memory, recall, second_brain, transcripts

    stores = []
    if transcripts.configured(env).enabled:
        stores += ["day transcripts", "records and profile"]
        if mem0_memory.configured(recall.configured(env), env).enabled and mem0_memory.available():
            stores.append("mem0 index (OpenAI)")
    if second_brain.configured(env).enabled:
        stores.append("second brain vault")
    return f"Memory: {_join(stores)}." if stores else "Memory: off."


def _doors(env: Mapping[str, str]) -> str:
    from . import ears, meet, miniapp, telegram, voice_agent

    tg = telegram.configured(env).enabled
    doors = (["Telegram"] if tg else []) + (["the Mini App"] if miniapp.configured(env).enabled else []) + (
        ["the Meet notetaker"] if tg and meet.configured(env).enabled else []) + (
        ["voice (wake word)"] if ears.configured(env).enabled and voice_agent.live_available(env) else [])
    return f"Doors on: {_join(doors)}." if doors else "Doors on: none."


def _canvas(env: Mapping[str, str]) -> list[str]:
    """One line while Canvas is set up (canvas.configured: CANVAS_BASE_URL and CANVAS_API_TOKEN), else none, so a
    eve without it has the block it had. The school's address is not named: the line says what, not where."""
    from . import canvas, telegram

    on = canvas.configured(env).enabled and telegram.configured(env).enabled
    return ["Canvas (school courses): on, read only."] if on else []


def _chief(env: Mapping[str, str]) -> list[str]:
    """One line while the chief of staff is on, else none (chief.enabled reads the same switch the daemon builds
    it from)."""
    from . import chief

    return ["Chief of staff: on."] if chief.enabled(env) else []


def _agentcard(env: Mapping[str, str]) -> list[str]:
    """Only capability state, with no token, address, cart or purchase history in the stable prompt."""
    from . import agentcard, openrouter, telegram

    try:
        on = (telegram.configured(env).enabled and openrouter.available(env)
              and agentcard.configured(env).enabled)
    except ValueError:
        on = False
    return ["AgentCard: chat/call purchases; exact cart needs owner approval."] if on else []


# apps_mcp's policies in words, grouped so one line carries every app (each app's name is apps_mcp.SERVERS' label)
_APP_RULES = (("read", "read only"), ("auto", "routine changes run, sends and deletes asked first"),
              ("write", "may change"), ("ask", "changes asked first"))


def _apps(env: Mapping[str, str]) -> list[str]:
    """The owner's apps, one line per kind, only while the Telegram brain (their only door) is on: the direct ones
    that are signed in, with their write rule; Composio's toolkits while it has a key and an owner."""
    from . import apps_mcp, composio_tools, telegram

    tg = telegram.configured(env)
    if not tg.enabled:
        return []
    out = []
    cfg = apps_mcp.configured(env)
    up = apps_mcp.AppsBridge(cfg).available() if cfg.enabled else []
    if up:
        rules = [f"{_join([apps_mcp.SERVERS[a].label for a in up if cfg.policy.get(a, 'ask') == rule])} {words}"
                 for rule, words in _APP_RULES if any(cfg.policy.get(a, "ask") == rule for a in up)]
        out.append(f"Own apps: {'; '.join(rules)}.")
    cc = composio_tools.configured(env, tg.owner_ids)
    if cc.enabled:
        kits = cc.toolkits if cc.toolkits is not None else ()
        out.append(f"Composio apps: {_join([k.capitalize() for k in kits])}." if kits else "Composio apps: all.")
    return out


def _sharing(env: Mapping[str, str]) -> list[str]:
    """File sharing, one line while on (with Telegram, its door)."""
    from . import share, telegram

    if not telegram.configured(env).enabled:
        return []
    out = []
    sc = share.configured(env)
    if sc.enabled:
        email = ("email, attached from Gmail (over 18 MB, a Drive link)" if share.gmail_can_send(env)
                 else "email as a Drive link")
        vias = (["iMessage"] if sc.imessage else []) + ([email] if share.google_token(env) else [])
        if vias:
            out.append(f"File sharing: {_join(vias)}, on the owner's Allow.")
    return out


def lines(environ: Optional[Mapping[str, str]] = None, *, lights_path: Optional[Path] = None,
          commit: Optional[str] = None) -> list[str]:
    """The block's lines, in order. `lights_path` and `commit` are lent by tests (the lights file and the sha)."""
    env = os.environ if environ is None else environ
    sha = running_commit() if commit is None else commit
    with _quiet():
        out = _models(env) + [_search(env), _computer(env), _watch(env), _lights(env, lights_path), _memory(env),
                              _doors(env)] + _canvas(env) + _apps(env) + _sharing(env) + _chief(env) + (
            _agentcard(env))
    if sha:
        out.append(f"Running code: git commit {sha}.")
    return out


def block(environ: Optional[Mapping[str, str]] = None, *, lights_path: Optional[Path] = None,
          commit: Optional[str] = None) -> str:
    """The block for a prompt: the header, "- " lines, the rule. "" when it cannot be built (it is context, and a
    door never fails over it) or while another configured() is building one (the doors read each other)."""
    if getattr(_building, "on", False):
        return ""
    _building.on = True
    try:
        body = lines(environ, lights_path=lights_path, commit=commit)
    except Exception as e:  # noqa: BLE001 — a missing optional part must never stop a door from opening
        log.warning("self context: not built (%s: %s)", type(e).__name__, e)
        return ""
    finally:
        _building.on = False
    text = HEADER + "\n" + "\n".join("- " + ln for ln in body) + "\n" + RULE
    if len(text) > BUDGET:
        log.info("self context: %d chars, over the %d budget", len(text), BUDGET)
    return text
