"""Shared web-search configuration for voice, text and deep reasoning.

Perplexity through OpenRouter is the default whenever ``OPENROUTER_API_KEY`` is set (owner, 2026-09-24: "use this
benchmark to wire in good realtime search"). On OpenRouter's search benchmarks Perplexity led BrowseComp, HLE and
WideSearch on quality, value and speed with the model held fixed (BrowseComp, 2026-08-18: 89.0% with Claude Opus 5
against 82.2% on Exa), and it costs $0.005 a search against Exa's $0.007. One OpenRouter chat call carries the
``openrouter:web_search`` server tool on the chosen engine, capped at ``max_uses`` searches; a cheap model writes the
short answer from the results, and the brain gets the answer and its sources through the ``web_search`` function tool.
(Until 2026-10-08 an OpenAI answer model was refused here, because OpenAI models ran on the OpenAI key; since the
owner moved every model call to his OpenRouter key, docs/openrouter.md, an ``openai/*`` answer model is allowed.)

Search engines do not serve a live clock (Exa could not say the time in Seattle, 2026-09-21, and Perplexity cannot
either, 2026-09-24), so the answer step is told the local date and time: a clock or "today" question is answered
from it, not from a page. Without an OpenRouter key there is no search (and no brain to ask for one): ``off``.
``openai`` (OpenAI's hosted ``web_search`` tool) is kept as an explicit choice only.
``EVE_WEB_SEARCH`` = openrouter-perplexity | openrouter-exa | openai | off picks one explicitly.

ROUTED (2026-09-29, search_router.py): when the search router is on (``EVE_SEARCH_ROUTER``; auto routes since its
gates passed a blind holdout on 2026-09-29), ``configured`` lists the providers it may use in ``SearchConfig.providers`` and ``search`` hands
each query to ``search_router.answer``: Jev sends a one-fact or one-page lookup to TinyFish (free), live fares and
official records to Firecrawl's Alexandria data tools, and everything else to the engine search below, which is also
where any failure lands. The result's shape and cost fields are the same whichever provider answered, and the brain's
``web_search`` tool description says what the routed search can do, so the brain knows its own reach. With
``providers`` empty, ``search`` is ``engine_search``: exactly the search this module made before the router.
A routed TinyFish or Firecrawl attempt has ``routed_secs`` (EVE_SEARCH_ROUTED_SECS, default 10 s) as an outer
budget; past it the engine search answers on its own ``timeout_secs``, so a routed search stays within about
Jev's 2 s + 10 s + 12 s, plus 8 s when the engine search falls back to the other engine. ``cost_usd`` is a float, or None when a routed answer could not be priced (its answer
model reported no cost, or Firecrawl credits have no price): None is "unpriced", never a partial sum. The engine
search keeps its pre-router figure: OpenRouter's usage.cost, else the search fee alone.

LAST RESORT (2026-10-08, monid.py, docs/monid.md): when both OpenRouter engines failed, Monid's Google results answer
in the same shape (``fallback`` "monid", an answer built from the top snippets, no model call), within the same
budget and under its own daily caps. EVE_MONID=0 switches it off.
"""

from __future__ import annotations

import concurrent.futures
import json
import logging
import os
import time
import urllib.error
import urllib.request
from dataclasses import dataclass, replace
from datetime import datetime
from typing import Any, Callable, Optional
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from . import openrouter, spend

log = logging.getLogger(__name__)

URL = "https://openrouter.ai/api/v1/chat/completions"
ENGINES = ("openrouter-perplexity", "openrouter-exa", "openai", "off")
OPENROUTER_ENGINES = {"openrouter-perplexity": "perplexity", "openrouter-exa": "exa"}   # eve's name -> OpenRouter's
DEFAULT_ENGINE = "openrouter-perplexity"   # with OPENROUTER_API_KEY; without it, "off"
# Selected for synthesis by the quality/cost screen (2026-10-09); watches and reflection keep their own defaults.
DEFAULT_MODEL = "openai/gpt-6-luna"
DEFAULT_RESULTS = 5
DEFAULT_MAX_USES = 2                       # searches per question: realtime answers, and at most $0.01 in search fees
# 2026-10-08 evening: both OpenRouter engines took 15-27 s per search for hours (normally 4-6 s, p95 5.1 s), and the
# 12 s + 8 s budget set that morning turned every search into "the search timed out" (owner: "everything was working
# on telegram", where the limit had been 20 s). Now the engines race: the second starts when the first is slow
# (HEDGE_SECS) or fails, the first good answer wins, and the whole search waits at most DEFAULT_TIMEOUT_SECS.
DEFAULT_TIMEOUT_SECS = 25.0                # the whole search, both engines
HEDGE_SECS = 6.0                           # past the normal p95: start the other engine too
FALLBACK_SECS = DEFAULT_TIMEOUT_SECS       # kept for callers that read it: the other engine gets what is left
FALLBACK_ENGINE = {"openrouter-perplexity": "openrouter-exa", "openrouter-exa": "openrouter-perplexity"}
DEFAULT_ROUTED_SECS = 10.0                 # a routed TinyFish/Firecrawl attempt, then the engine on its own timeout
MAX_ANSWER_CHARS = 1500
MAX_SNIPPET_CHARS = 300
MAX_SOURCES = 8
# Per search, up to 10 results (openrouter.ai/docs server-tools/web-search, 2026-09-24); the model's tokens come on
# top. Used only when OpenRouter's reply carries no usage.cost.
SEARCH_USD = {"perplexity": 0.005, "exa": 0.007}

TOOL_NAME = "web_search"
WEB_SEARCH_TOOL: dict[str, Any] = {
    "type": "function", "name": TOOL_NAME, "strict": True,
    "description": "Search the web for current facts: news, scores, prices, weather, opening hours, anything after "
                   "your training. Returns a short answer and the sources it came from. The current date and time are "
                   "already in your context; a search is not needed for them.",
    "parameters": {"type": "object", "additionalProperties": False, "required": ["query"],
                   "properties": {"query": {"type": "string",
                                            "description": "What to look up, as a search query in plain words."}}},
}
HOSTED_TOOL: dict[str, Any] = {"type": "web_search"}      # OpenAI's own, the default

# What the routed search adds to the tool's description, per provider it may use: the brain reads its own reach here
# (a brain that does not know its search can price a flight never asks it to).
ROUTED_REACH = {
    "tinyfish": "a single fact or what one specific page says is read straight from the pages",
    "firecrawl": "live flight fares and official records come from live data services, so give the route, dates or "
                 "name in the query",
}
ROUTED_NOTE = ("Each query is routed to the search that fits it: news and questions that need many sources get a "
               "many-source search")

SYSTEM = ("You are the answer step of eve's web search: eve, the owner's desk-robot assistant, searched the web "
          "for this query. Using only the web results provided, answer the query in at most four plain sentences "
          "with the concrete facts (numbers, names, dates) and say which source each comes from by its title. If the "
          "results do not answer it, say so in one sentence. No markdown, no lists.")
# The clock the answer step reads: web pages are not a live clock, so the current time comes from here.
CLOCK = ("The current local date and time is {now}, time zone {zone}. For a question about the current time or date, "
         "or what is happening today, use this clock, converting to another place's time zone when asked; do not "
         "take the time from a web page.")


@dataclass(frozen=True)
class SearchConfig:
    engine: str = "openai"                    # a bare config (no environment read) never needs an OpenRouter key
    model: str = DEFAULT_MODEL
    max_results: int = DEFAULT_RESULTS
    timeout_secs: float = DEFAULT_TIMEOUT_SECS
    max_uses: int = DEFAULT_MAX_USES
    providers: tuple[str, ...] = ()           # search_router's providers when routing is on; () = the engine alone
    routed_secs: float = DEFAULT_ROUTED_SECS  # outer budget of a routed non-engine attempt (search_router.bounded)


def configured(environ: Any = None) -> SearchConfig:
    """EVE_WEB_SEARCH = openrouter-perplexity | openrouter-exa | openai | off. Unset: openrouter-perplexity
    when OPENROUTER_API_KEY is set, else off. EVE_WEB_SEARCH_MODEL, _RESULTS and _MAX_USES tune the call;
    any OpenRouter model id answers, openai/* included (every model call is on OpenRouter since 2026-10-08).
    ``providers`` is search_router.available: () unless EVE_SEARCH_ROUTER routes on this engine.
    EVE_SEARCH_ROUTED_SECS (1-60, default 10) is a routed attempt's outer budget."""
    from . import search_router

    env = os.environ if environ is None else environ
    key = (env.get("OPENROUTER_API_KEY") or "").strip()
    raw = (env.get("EVE_WEB_SEARCH") or "").strip().lower()
    if raw and raw not in ENGINES:
        log.warning("web search: EVE_WEB_SEARCH=%r is not one of %s; using the default", raw, ENGINES)
        raw = ""
    engine = raw or (DEFAULT_ENGINE if key else "off")
    if engine in OPENROUTER_ENGINES and not key:
        log.warning("web search: asked for %s but OPENROUTER_API_KEY is not set; search is off", engine)
        engine = "off"
    model = (env.get("EVE_WEB_SEARCH_MODEL") or DEFAULT_MODEL).strip()
    try:
        n = max(1, min(10, int(env.get("EVE_WEB_SEARCH_RESULTS") or DEFAULT_RESULTS)))
    except ValueError:
        n = DEFAULT_RESULTS
    try:
        uses = max(1, min(5, int(env.get("EVE_WEB_SEARCH_MAX_USES") or DEFAULT_MAX_USES)))
    except ValueError:
        uses = DEFAULT_MAX_USES
    try:
        routed = max(1.0, min(60.0, float(env.get("EVE_SEARCH_ROUTED_SECS") or DEFAULT_ROUTED_SECS)))
    except ValueError:
        routed = DEFAULT_ROUTED_SECS
    return SearchConfig(engine=engine, model=model, max_results=n, max_uses=uses,
                        providers=search_router.available(env, engine), routed_secs=routed)


def tools_for(config: SearchConfig) -> list[dict[str, Any]]:
    """The tool(s) a brain offers for the web: the function tool, the hosted one, or nothing."""
    if config.engine in OPENROUTER_ENGINES:
        return [routed_tool(config.providers) if config.providers else WEB_SEARCH_TOOL]
    if config.engine == "openai":
        return [HOSTED_TOOL]
    return []


def routed_tool(providers: tuple[str, ...]) -> dict[str, Any]:
    """WEB_SEARCH_TOOL with its description telling the brain what the routed search reaches."""
    reach = [ROUTED_REACH[p] for p in providers if p in ROUTED_REACH]
    note = ROUTED_NOTE + "".join(f"; {r}" for r in reach) + "."
    return {**WEB_SEARCH_TOOL, "description": WEB_SEARCH_TOOL["description"] + " " + note}


def _clock(environ: Any = None, now: Optional[datetime] = None) -> str:
    env = os.environ if environ is None else environ
    zone_name = (env.get("EVE_TIMEZONE") or "").strip()
    try:
        zone = ZoneInfo(zone_name) if zone_name else None
    except (ZoneInfoNotFoundError, ValueError):
        zone = None
    local = (now or datetime.now().astimezone()).astimezone(zone)
    return CLOCK.format(now=local.strftime("%A %Y-%m-%d %H:%M"), zone=zone.key if zone else local.tzname())


def answer_options(model: str, max_tokens: int) -> dict[str, Any]:
    """Reasoning models need room for both thinking and the short answer; keep older override budgets."""
    model_id = openrouter.model(model)
    if model_id.startswith("openai/gpt-6"):
        return {"max_tokens": max(2048, max_tokens), "reasoning": {"effort": "low"}}
    if model_id == "google/gemini-3.8-flash":
        return {"max_tokens": max(2048, max_tokens), "temperature": 0, "reasoning": {"effort": "low"}}
    if model_id == "anthropic/claude-haiku-5.5":
        return {"max_tokens": max_tokens}
    if model_id.startswith("inception/"):
        # Mercury reasons by default; low effort keeps the answer one burst (79 reasoning tokens, 2026-10-19)
        return {"max_tokens": max(1024, max_tokens), "reasoning": {"effort": "low"}}
    return {"max_tokens": max_tokens, "temperature": 0}


def request(config: SearchConfig, query: str, *, environ: Any = None, now: Optional[datetime] = None,
            system: str = SYSTEM) -> dict[str, Any]:
    """The exact OpenRouter body: the cheap answer model, the web search server tool on the engine with its budget,
    the clock and the answer-box rules (``system``) as the system turn, the query as the user turn."""
    return {
        "model": config.model,
        "tools": [{"type": "openrouter:web_search",
                   "parameters": {"engine": OPENROUTER_ENGINES.get(config.engine, "perplexity"),
                                  "max_results": config.max_results, "max_uses": config.max_uses}}],
        "messages": [{"role": "system", "content": system + " " + _clock(environ, now)},
                     {"role": "user", "content": " ".join((query or "").split())[:500]}],
        **answer_options(config.model, 400),
        "usage": {"include": True},
        **({"provider": prefs} if (prefs := openrouter.provider_prefs(config.model)) else {}),
    }


def parse(payload: Any) -> dict[str, Any]:
    """{"ok", "answer", "sources": [{"title", "url", "snippet"}], "usage"} from a chat completion. A reply
    with no annotations is still an answer; a reply with no text is not."""
    if not isinstance(payload, dict) or not isinstance(payload.get("choices"), list) or not payload["choices"]:
        raise ValueError("no choices in the reply")
    message = (payload["choices"][0] or {}).get("message") or {}
    answer = " ".join(str(message.get("content") or "").split())[:MAX_ANSWER_CHARS]
    sources: list[dict[str, str]] = []
    seen: set[str] = set()
    for ann in message.get("annotations") or []:
        cite = ann.get("url_citation") if isinstance(ann, dict) else None
        if not isinstance(cite, dict):
            continue
        url = str(cite.get("url") or "").strip()
        if not url or url in seen:
            continue
        seen.add(url)
        sources.append({"title": " ".join(str(cite.get("title") or "").split())[:120], "url": url[:500],
                        "snippet": " ".join(str(cite.get("content") or "").split())[:MAX_SNIPPET_CHARS]})
        if len(sources) >= MAX_SOURCES:
            break
    if not answer and not sources:
        raise ValueError("the reply had neither an answer nor a source")
    usage = payload.get("usage") if isinstance(payload.get("usage"), dict) else {}
    out = {"ok": True, "answer": answer, "sources": sources,
           "usage": {"in": int(usage.get("prompt_tokens") or 0), "out": int(usage.get("completion_tokens") or 0)}}
    if isinstance(usage.get("cost"), (int, float)):
        out["usage"]["cost"] = float(usage["cost"])          # OpenRouter's own figure: searches and tokens together
    return out


def search(query: str, config: Optional[SearchConfig] = None, *, key: str = "",
           opener: Optional[Callable[..., Any]] = None, clock: Callable[[], float] = time.perf_counter) -> dict[str, Any]:
    """The brain's web search, blocking (callers run it off the loop). Never raises: {"ok": False, "reason": …}
    instead. With ``config.providers`` it is search_router.answer (Jev picks the provider; the same result shape);
    without, engine_search. `key` defaults to OPENROUTER_API_KEY. Logs the shape and the time, never the query."""
    cfg = config or configured()
    if cfg.providers:
        from . import search_router

        return search_router.answer(query, cfg, key=key, opener=opener, clock=clock)
    return engine_search(query, cfg, key=key, opener=opener, clock=clock)


MONID_RESERVE_SECS = 10.0                # of the budget, kept for Monid when it is on: it can then cover engines that hang
MONID_HTTP = (401, 402, 403, 429)        # OpenRouter refusing the account or the rate: Monid can still answer


def engine_search(query: str, config: Optional[SearchConfig] = None, *, key: str = "",
                  opener: Optional[Callable[..., Any]] = None, clock: Callable[[], float] = time.perf_counter,
                  system: str = SYSTEM, feature: str = spend.SEARCH, monid_ok: bool = True) -> dict[str, Any]:
    """One search on the engine (Perplexity through OpenRouter by default), the search before the router and the
    router's safe default. ``system`` is the answer step's rules; ``feature`` the spend line (the web reader's
    searches are its tasks). Hedged: when the first engine has not answered in HEDGE_SECS, or fails in a way worth
    another try (a timeout, no route, 429/5xx), the other OpenRouter engine starts too, and the first good answer
    wins; the result then says ``fallback`` when it was the other engine's. The whole search waits at most
    ``config.timeout_secs``. A slower loser keeps running in the background until its own timeout. When BOTH
    engines failed (and neither with a 4xx other than 429), Monid's Google results (monid.py) get one try with
    what is left of the budget, at most 12 s, and answer with ``fallback`` "monid"; otherwise, or when Monid
    cannot answer, the first engine's failure is returned. Never raises."""
    cfg = config or configured()
    other = FALLBACK_ENGINE.get(cfg.engine)

    def once(c: SearchConfig) -> dict[str, Any]:
        return _engine_once(query, c, key=key, opener=opener, clock=clock, system=system, feature=feature)

    def clean(out: dict[str, Any]) -> dict[str, Any]:
        out.pop("transient", None)
        out.pop("failed_ms", None)
        out.pop("http", None)
        return out

    def last_resort(failures: list[dict[str, Any]], started: float, deadline: float) -> dict[str, Any]:
        """Both engines failed: Monid once with what is left of the budget (monid.py, at most 12 s), else the
        first engine's failure. Never after a 4xx other than 429 (a request the engines refused is not retried)."""
        original = clean(dict(failures[0]))
        if any(400 <= int(f.get("http") or 0) < 500 and f.get("http") not in MONID_HTTP for f in failures):
            return original
        if not use_monid:
            return original
        from . import monid
        log.info("web search: both engines failed; trying Monid")
        out = monid.web_search(query, deadline - time.monotonic())
        if not out.get("ok"):
            return original
        out["ms"] = round((time.monotonic() - started) * 1000.0)              # the whole wait, engines and Monid
        return out

    if other is None:
        return clean(once(cfg))
    budget = cfg.timeout_secs
    from . import monid

    use_monid = monid_ok and monid.mode() != "off"
    # with Monid on, the engines race for the budget less Monid's reserve, so a hang still leaves it time
    race = budget - MONID_RESERVE_SECS if use_monid and budget > MONID_RESERVE_SECS + HEDGE_SECS else budget
    pool = concurrent.futures.ThreadPoolExecutor(max_workers=2, thread_name_prefix="websearch")
    try:
        started = time.monotonic()
        first = pool.submit(once, cfg)
        done, _ = concurrent.futures.wait([first], timeout=min(HEDGE_SECS, race))
        if done:
            out = first.result()
            if use_monid and out.get("http") in MONID_HTTP[:3]:
                # OpenRouter refused the account (401/402/403): the other engine is the same account, so straight
                # to the last resort
                return last_resort([out, out], started, started + budget)
            if out.get("ok") is not False or not out.get("transient"):
                return clean(out)
        hedge_at = time.monotonic() - started
        left = max(1.0, race - hedge_at)
        log.info("web search: %s %s; starting %s too", cfg.engine,
                 "failed (%s)" % first.result().get("reason") if done else "is slow", other)
        second = pool.submit(once, replace(cfg, engine=other, timeout_secs=left))
        pending = {second} if done else {first, second}
        first_failure: Optional[dict[str, Any]] = first.result() if done else None
        second_failure: Optional[dict[str, Any]] = None
        deadline = started + race
        while pending:
            got, pending = concurrent.futures.wait(pending, timeout=max(0.0, deadline - time.monotonic()),
                                                   return_when=concurrent.futures.FIRST_COMPLETED)
            if not got:
                break
            for f in got:
                out = f.result()
                if out.get("ok") is not False:
                    out = clean(out)
                    if f is second:
                        out["fallback"] = other
                        out["ms"] = round(hedge_at * 1000.0 + out.get("ms", 0))   # the whole wait, both engines
                    return out
                if f is first:
                    first_failure = out
                else:
                    second_failure = out
        if first_failure is not None and second_failure is not None:
            return last_resort([first_failure, second_failure], started, started + budget)
        timed_out = {"ok": False, "reason": "the search timed out", "transient": True}
        if use_monid:
            log.warning("web search: no engine answered in %.0f s", race)
            return last_resort([first_failure or timed_out, second_failure or timed_out], started, started + budget)
        if first_failure is not None:
            return clean(first_failure)
        log.warning("web search: no engine answered in %.0f s", budget)
        return {"ok": False, "reason": "the search timed out"}
    finally:
        pool.shutdown(wait=False, cancel_futures=True)


def _engine_once(query: str, cfg: SearchConfig, *, key: str, opener: Optional[Callable[..., Any]],
                 clock: Callable[[], float], system: str, feature: str) -> dict[str, Any]:
    """engine_search's one attempt. A failure worth one more try on the other engine carries ``transient``."""
    api_key = key or (os.environ.get("OPENROUTER_API_KEY") or "").strip()
    if not api_key:
        return {"ok": False, "reason": "web search is not set up on this computer (no OpenRouter key)"}
    if not (query or "").strip():
        return {"ok": False, "reason": "empty query"}
    send = opener or urllib.request.urlopen
    body = json.dumps(request(cfg, query, system=system)).encode("utf-8")
    req = urllib.request.Request(URL, data=body, method="POST", headers={
        "Authorization": f"Bearer {api_key}", "Content-Type": "application/json",
        "HTTP-Referer": "https://github.com/gurucharan/eve", "X-Title": "eve"})
    t0 = clock()
    try:
        with send(req, timeout=cfg.timeout_secs) as response:
            payload = json.loads(response.read().decode("utf-8"))
        result = parse(payload)
    except urllib.error.HTTPError as e:
        log.warning("web search: HTTP %s from OpenRouter", e.code)
        return {"ok": False, "reason": f"the search service answered HTTP {e.code}", "http": e.code,
                "transient": e.code == 429 or e.code >= 500, "failed_ms": (clock() - t0) * 1000.0}
    except urllib.error.URLError as e:
        log.warning("web search: OpenRouter unreachable (%s)", type(e.reason).__name__)
        return {"ok": False, "reason": "the search service is unreachable", "transient": True,
                "failed_ms": (clock() - t0) * 1000.0}
    except (TimeoutError, OSError):
        log.warning("web search: timed out after %.0f s", cfg.timeout_secs)
        return {"ok": False, "reason": "the search timed out", "transient": True,
                "failed_ms": (clock() - t0) * 1000.0}
    except (ValueError, TypeError) as e:
        log.warning("web search: unreadable reply (%s)", e)
        return {"ok": False, "reason": "the search service sent an unreadable reply"}
    ms = (clock() - t0) * 1000.0
    result["ms"] = round(ms)
    engine = OPENROUTER_ENGINES.get(cfg.engine, "perplexity")
    measured = result["usage"].get("cost")
    result["cost_usd"] = round(measured if measured is not None else SEARCH_USD.get(engine, 0.005), 4)
    # The daily meter takes OpenRouter's own figure only: without it the answer model's tokens are unknown, so
    # the line is unpriced rather than the search fee alone passed off as the whole cost.
    spend.record("openrouter", cfg.model, feature, measured,
                 tokens={"in": result["usage"].get("in"), "out": result["usage"].get("out")},
                 source="reported" if measured is not None else "priced",
                 note="" if measured is not None else f"no usage.cost; search fee alone ~${SEARCH_USD.get(engine, 0.005)}")
    log.info("web search: %d sources in %.0f ms via %s on %s, $%.4f", len(result["sources"]), ms, cfg.model, engine,
             result["cost_usd"])
    return result
