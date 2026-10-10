"""Which provider answers a web search: a quick page reader, a many-source search engine, or live data services.

Every web search eve makes goes through ``answer`` here: the brain's ``web_search`` tool (websearch.search, for
voice, Telegram and think) and the web reader body (web_reader.WebReaderAgent, for a public-web task). Three
providers, each good at a different job (2026-09-29):

* **tinyfish** (tinyfish.py, web_reader.search_and_read + answer_from): free (docs.tinyfish.ai, 2026-09-29).
  Search returns titles, snippets and links; Fetch reads the top pages; a cheap model answers only from those
  pages. Good for one fact, opening hours, a definition, a price on a public page, what one page or doc says, a
  simple comparison from a few pages. Weak at many-source synthesis, breaking news and multi-hop research.
* **perplexity** (websearch.engine_search): OpenRouter's web_search server tool, about $0.005 a search plus the
  answer model; it led OpenRouter's BrowseComp, HLE and WideSearch benchmarks (websearch.py). Good for synthesis
  across many sources, current events, multi-step research and anything ambiguous. The safe default.
* **firecrawl** (firecrawl.py): paid credits, with Alexandria's live data tools (Google Flights and Skyscanner
  fares, government records, ...). Worth it only when a data tool fits: live fares, prices from a data service,
  official records.

Jev decides, asked the way browser_router.py asks it (docs.typesafe.ai: State, Choice "Structured instructions
and criteria", Intent routing; typed_ask.py's idiom):

* The state is the request, beside one line saying who is asking and what the text is (a search query eve's
  brain wrote, or the owner's own request) — no provider manuals in the state ("context rot").
* ALL THREE providers are the options of one Choice, on every Mac, whichever keys it holds (2026-09-29: the
  question never varies, so the gates fitted on one machine hold on every other; a pick whose key is missing is
  masked in code to perplexity). Each option is written as ``what``, ``for`` and ``not_for``, the boundary in
  both sides. The wording names no brand, so Jev reads the job, and a provider can change behind a key without a
  refit.
* Beside the Choice, four absolute Nouls, one judgment each: does it need live structured data a data service
  holds? does the correct answer change over time or hang on recent news (so a page may be stale)? does it need
  many sources weighed together or several searches in a row? can one or two ordinary pages answer it?
* Code combines them and owns the rule: firecrawl only when its Choice probability AND the structured-data Noul
  clear their fitted gates; tinyfish only when its probability AND the single-lookup Noul clear theirs AND both
  the changes-over-time Noul and the many-sources Noul are under their maximums; everything else is perplexity.
  An error, a timeout, a missing key, a provider that fails or finds nothing, today's Firecrawl cap spent:
  perplexity. A wrong route is a request labelled perplexity or firecrawl sent to tinyfish (a thin answer from
  too few pages), or anything not labelled firecrawl sent to firecrawl (paid credits for pages a free reader
  reads). Every cut-off is fitted with ZERO wrong routes allowed on the seen sets (tools/route_eval.py --search:
  search_tuning.json + search_holdout.json) and scored once on the blind search_holdout2.json against the bar
  pre-registered beside SHIPPED.

Time (2026-09-29): a routed tinyfish or firecrawl attempt runs in a worker under an outer budget
(``SearchConfig.routed_secs``, EVE_SEARCH_ROUTED_SECS, 10 s for the brain; READER_ROUTED_SECS for the web
reader, whose tasks may take longer); past it the router stops waiting and perplexity answers with its own
timeout: the brain's whole search is at most about 2 s (Jev) + 10 s + the engine's 12 s, plus 8 s when it
falls back to the other OpenRouter engine (websearch.engine_search). Firecrawl fare answers
measured 6.9-7.8 s end to end (three live questions, 2026-09-29), inside the 10 s. Cost: ``cost_usd`` is a float only when
every part of the answer was priced; a Firecrawl credit with no price (EVE_FIRECRAWL_USD unset) or an answer
model that reported no cost makes it None ("unpriced", as spend.py records it), never a partial figure.

``SHIPPED`` is True since the 2026-09-29 blind holdout passed its pre-registered bar (below GATES), so
``EVE_SEARCH_ROUTER=auto`` routes whenever a second provider is usable. ``off`` leaves every search exactly as it
was: the brain on websearch's engine, the web reader on TinyFish.
"""

from __future__ import annotations

import contextvars
import logging
import os
import re
import threading
import time
from dataclasses import dataclass
from typing import Any, Callable, Mapping, Optional

from . import firecrawl, spend, tinyfish, watch, websearch

log = logging.getLogger(__name__)

Predict = Callable[[Any, dict[str, Any]], dict[str, Any]]

TINYFISH, PERPLEXITY, FIRECRAWL = "tinyfish", "perplexity", "firecrawl"
PROVIDERS = (PERPLEXITY, TINYFISH, FIRECRAWL)
MODES = ("auto", "on", "off")
JEV_TIMEOUT_S = 2.0                  # a search is seconds; a slow router is a perplexity search, not a wait
# The web reader's routed attempt, at least: its tasks may take a Firecrawl search (30 s), a data tool (45 s) and
# two model calls; past it the engine search, then Codex. The brain's is SearchConfig.routed_secs (10 s).
READER_ROUTED_SECS = 90.0

# The three providers, as the Choice's options. Facts only, from each provider's own docs and this repo's use of it
# (tinyfish.py, websearch.py, firecrawl.py); the keys and the words name the job, never the brand.
OPTIONS: dict[str, Any] = {
    "quick_page_read": {
        "what": "a quick reader that runs one web search, reads the top few result pages, and answers only from "
                "what those few pages say",
        "for": ["one settled fact, number, date or definition that stays the same over time",
                "opening hours, a phone number, an address, or a price shown on a public web page",
                "what one specific public page, documentation page, policy or article says",
                "a simple comparison of two or three things whose facts sit on a few pages"],
        "not_for": ["questions that need many sources weighed together: reviews, recommendations, 'best' lists",
                    "anything whose correct answer changes over time or depends on recent news: who holds a role "
                    "now, the latest result or release, today's events",
                    "research that takes several searches one after another", "vague or open-ended questions",
                    "live fares, bookable prices or official records held by a data service"],
    },
    "deep_web_research": {
        "what": "a search engine that searches the web several times, reads many sources, and writes one answer "
                "that brings them together, with citations",
        "for": ["questions that need many sources weighed together: reviews, recommendations, pros and cons",
                "anything whose correct answer changes over time: who holds a role now, the latest result or "
                "release, current events, news, and anything that happened today or this week",
                "research that takes several steps or searches", "vague, broad or ambiguous questions",
                "anything that fits none of the other options"],
        "not_for": ["one settled fact, definition, opening hours or page statement that one or two ordinary "
                    "public pages give and that stays the same over time",
                    "live flight fares or bookable prices that a travel or fare-comparison service holds",
                    "a lookup in an official register or government records database"],
    },
    "live_data_service": {
        "what": "live data services queried directly with structured fields, such as flight search, fare "
                "comparison and government or official records lookups, answering from their own data rather "
                "than from web pages",
        "for": ["live flight fares or schedules, or the cheapest flight between two places on a date",
                "hotel, travel or ticket prices for given dates from a booking or fare-comparison service",
                "official records: company registrations, government registers, licences, public filings"],
        "not_for": ["facts, definitions, hours, news or summaries that ordinary web pages state",
                    "a request with no route, dates, name or identifier for a service to look up"],
    },
}
OPTION_TO_PROVIDER = {"quick_page_read": TINYFISH, "deep_web_research": PERPLEXITY, "live_data_service": FIRECRAWL}
PROVIDER_TO_OPTION = {v: k for k, v in OPTION_TO_PROVIDER.items()}

# Who is asking, in one line: the text is sometimes a terse query the brain wrote, sometimes the owner's own words.
ABOUT = ("eve, the owner's desk-robot assistant, is about to search the web. The request is either a search query "
         "eve wrote while talking with the owner, or the owner's own words; judge what answering it needs.")


def questions() -> dict[str, Any]:
    """The Choice over ALL THREE providers and the four Nouls: the same question on every Mac, whichever keys it
    holds (a pick that is not available is masked in make_router, never hidden from Jev), so fitted gates hold."""
    return {
        "provider": {"type": "choice", "instructions": "Which of these should answer the request?",
                     "criteria": {PROVIDER_TO_OPTION[p]: OPTIONS[PROVIDER_TO_OPTION[p]] for p in PROVIDERS}},
        "needs_live_structured_data": {"type": "noul", "instructions": (
            "Does answering need live data that a booking, fare-comparison or official records service holds and "
            "would be asked with specific fields: flight or train fares or schedules between two places, hotel or "
            "ticket prices for given dates, or a lookup in a government or official register? Answer no for "
            "facts, prices, hours or news that an ordinary web page states.")},
        "answer_changes_over_time": {"type": "noul", "instructions": (
            "Does the correct answer change over time, or depend on news or events of the last few weeks, so that "
            "a web page written some months ago could now give a wrong answer? Yes for who holds a job or title "
            "now, the latest version, result or standing, and current events. No for a settled fact that stays "
            "the same, such as a definition, a date in history, or what a named document says.")},
        "needs_many_sources_or_searches": {"type": "noul", "instructions": (
            "Does answering need many sources weighed together (reviews, recommendations, pros and cons, 'best' "
            "lists), or several searches one after another, each depending on what the last one found?")},
        "single_lookup_or_page_read": {"type": "noul", "instructions": (
            "Can the whole answer come from one or two ordinary public web pages: a single fact, number, date, "
            "definition, opening hours, phone number, a price shown on a page, or what one specific named page or "
            "document says?")},
    }


@dataclass(frozen=True)
class SearchAnswer:
    p_tinyfish: float                # the Choice's probability for each provider's option
    p_perplexity: float
    p_firecrawl: float
    structured: float                # needs_live_structured_data
    many: float                      # needs_many_sources_or_searches
    single: float                    # single_lookup_or_page_read
    changes: float = 0.0             # answer_changes_over_time
    ms: float = 0.0
    error: str = ""


@dataclass(frozen=True)
class SearchGates:
    firecrawl: float = 1.01          # the Choice's probability for firecrawl, at least
    structured: float = 1.01         # the structured-data Noul, at least
    tinyfish: float = 1.01           # the Choice's probability for tinyfish, at least
    single: float = 1.01             # the single-lookup Noul, at least
    many_max: float = 0.0            # the many-sources Noul, at most, for tinyfish
    changes_max: float = 0.0         # the changes-over-time Noul, at most, for tinyfish


@dataclass(frozen=True)
class Route:
    provider: str
    p: float = 0.0                   # the Choice's probability for the provider chosen
    ms: float = 0.0                  # Jev's time
    error: str = ""


def ask(predict: Predict, text: str, clock: Callable[[], float]) -> SearchAnswer:
    """Jev's answers to questions(), always the full three-option question."""
    t0 = clock()
    try:
        result = predict({"about": ABOUT, "request": " ".join(str(text).split())[:500]}, questions())
        answers = result.get("answers") or {}
    except Exception as e:  # noqa: BLE001 — a model that fails abstains: perplexity, the safe default
        return SearchAnswer(0.0, 1.0, 0.0, structured=0.0, many=1.0, single=0.0, changes=1.0,
                            ms=(clock() - t0) * 1000.0, error=f"{type(e).__name__}: {e}"[:160])

    def noul(key: str) -> float:
        v = (answers.get(key) or {}).get("noul")
        return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else 0.0

    probs = (answers.get("provider") or {}).get("probabilities") or {}

    def p(provider: str) -> float:
        v = probs.get(PROVIDER_TO_OPTION[provider])
        return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else 0.0

    return SearchAnswer(p_tinyfish=p(TINYFISH), p_perplexity=p(PERPLEXITY), p_firecrawl=p(FIRECRAWL),
                        structured=noul("needs_live_structured_data"), many=noul("needs_many_sources_or_searches"),
                        single=noul("single_lookup_or_page_read"), changes=noul("answer_changes_over_time"),
                        ms=(clock() - t0) * 1000.0)


def decide(a: SearchAnswer, g: SearchGates) -> str:
    """FIRECRAWL or TINYFISH only when their conditions hold; anything else, including an error, is PERPLEXITY."""
    if a.error:
        return PERPLEXITY
    if a.p_firecrawl >= g.firecrawl and a.structured >= g.structured:
        return FIRECRAWL
    if (a.p_tinyfish >= g.tinyfish and a.single >= g.single and a.many <= g.many_max
            and a.changes <= g.changes_max):
        return TINYFISH
    return PERPLEXITY


def wrong(said: str, truth: str) -> bool:
    """A route that costs: a request that is not a quick read sent to the reader, or anything that does not need a
    data service sent to the paid one. Sending anything to perplexity is never wrong, only slower or dearer."""
    return (said == TINYFISH and truth != TINYFISH) or (said == FIRECRAWL and truth != FIRECRAWL)


GRID = (0.5, 0.6, 0.7, 0.8, 0.9, 0.95)
MAX_GRID = (0.05, 0.1, 0.2, 0.3, 0.5)


def fit(answers: list[SearchAnswer], truths: list[str]) -> SearchGates:
    """The cut-offs that route the most requests away from perplexity, each to its labelled provider, with ZERO
    wrong routes. Ties go to the stricter setting. The two branches are independent (firecrawl is checked first,
    and a request it takes never reaches the tinyfish test), so firecrawl is fitted first and tinyfish under it:
    its probability and single-lookup minimums, and its many-sources and changes-over-time maximums."""
    fc_best: Optional[tuple[float, ...]] = None
    fc = (1.01, 1.01)
    for p_fc in GRID:
        for structured in GRID:
            g = SearchGates(p_fc, structured)
            said = [decide(a, g) for a in answers]
            if any(wrong(s, t) for s, t in zip(said, truths, strict=True)):
                continue
            key = (sum(1 for s, t in zip(said, truths, strict=True) if s == t == FIRECRAWL), p_fc, structured)
            if fc_best is None or key > fc_best:
                fc_best, fc = key, (p_fc, structured)
    best: Optional[tuple[float, ...]] = None
    chosen = SearchGates(fc[0], fc[1])
    for p_tf in GRID:
        for single in GRID:
            for many in MAX_GRID:
                for changes in MAX_GRID:
                    g = SearchGates(fc[0], fc[1], p_tf, single, many, changes)
                    said = [decide(a, g) for a in answers]
                    if any(wrong(s, t) for s, t in zip(said, truths, strict=True)):
                        continue
                    right = sum(1 for s, t in zip(said, truths, strict=True) if s == t != PERPLEXITY)
                    key = (right, p_tf, single, -many, -changes)
                    if best is None or key > best:
                        best, chosen = key, g
    return chosen


# History (2026-09-29): a first fit on search_tuning.json alone, scored once on the blind search_holdout.json
# (80 requests), routed 43 away from perplexity with one wrong route: "who's the CEO of OpenAI" (labelled
# perplexity: the answer changes) sent to tinyfish at p 0.99, single 0.94, many 0.07. That miss split the old
# many-sources-or-news Noul in two (answer_changes_over_time, needs_many_sources_or_searches), put the quick-read
# boundary into deep_web_research.not_for too, and fixed the question to all three options on every Mac. That
# holdout is seen data now: the next fit is on search_tuning.json + search_holdout.json and scores ONCE on the
# blind search_holdout2.json.
#
# Refit and scored 2026-09-29 (tools/route_eval.py --search, live Jev on EVE_JEV_ROUTE, run once): fitted
# with zero wrong routes on the 147 seen requests (search_tuning.json 67 + search_holdout.json 80), then scored
# ONCE on search_holdout2.json, written blind AFTER the wording change above and never read before this run.
# n=90 (labelled tinyfish 32, firecrawl 18, perplexity 40). Routed to tinyfish 22, right 21 (precision 95.5%);
# to firecrawl 15, right 15; to perplexity 53, right 39. Away from perplexity 37. Coverage: 36 of the 50
# non-perplexity requests went to their labelled provider (21/32 tinyfish, 15/18 firecrawl); 75 of 90 right
# overall. Jev latency p50 189 ms, p95 313 ms, 0 errors. The one wrong route, verbatim: "what's the tallest
# building in the world": said tinyfish, truth perplexity (tinyfish 0.97 firecrawl 0.00 structured 0.04 changes
# 0.37 many 0.06 single 0.92). Bar: (a) 37 >= 20 pass, (b) 0 wrong to firecrawl pass, (c) 95.5% >= 95% pass:
# SHIPPED. Most misses are page reads (prices, hours on a named site) whose changes-over-time Noul sits above 0.5;
# they fall to perplexity, which is slower, never wrong. Nothing was tuned after seeing it. Caveat found in review:
# holdout2 repeats two seen requests word for word ("what time does the trader joe's on masonic open tomorrow", a
# miss; "how many ounces in a cup", a right tinyfish route) and nearly repeats the CEO case. Without the two exact
# repeats (c) is 20 of 21, 95.2%: still a pass, by one route. A new blind set should be written before any refit.
GATES = SearchGates(firecrawl=0.95, structured=0.7, tinyfish=0.8, single=0.6, many_max=0.1, changes_max=0.5)

# PRE-REGISTERED BAR (written 2026-09-29 before any scoring of the fresh holdout): on the fresh blind holdout,
# (a) at least 20 requests routed away from perplexity, (b) zero wrong routes to firecrawl, (c) precision on
# routes to tinyfish >= 95%. Pass all three -> SHIPPED=True. Otherwise SHIPPED=False. Scored exactly once; nothing
# tuned after seeing it. tools/route_eval.py --search prints this verdict. EVE_SEARCH_ROUTER=auto routes only
# when True. Scored 2026-09-29 on search_holdout2.json (n=90): (a) 37 routed, (b) 0 wrong to firecrawl, (c) 21 of 22
# to tinyfish right, 95.5%. All three pass.
SHIPPED = True


def make_router(predict: Predict, clock: Callable[[], float],
                gates: Optional[SearchGates] = None) -> Callable[[str, tuple[str, ...]], Route]:
    """``route(text, available) -> Route``: Jev's answer to the full question under the gates (the module's GATES
    when none are given, read at call time so a refit applies), then masked: a pick that is not ``available`` is
    perplexity (the question itself never changes with the keys, so the fitted gates hold on every Mac)."""
    def route(text: str, available: tuple[str, ...]) -> Route:
        a = ask(predict, text, clock)
        said, error = decide(a, gates or GATES), a.error
        if said not in available:
            said, error = PERPLEXITY, error or f"{said} is not available"
        p = {TINYFISH: a.p_tinyfish, FIRECRAWL: a.p_firecrawl, PERPLEXITY: a.p_perplexity}[said]
        return Route(said, p, a.ms, error)
    return route


_JEV: dict[str, Any] = {}
_JEV_LOCK = threading.Lock()


def jev_router(environ: Optional[Mapping[str, str]] = None) -> Optional[Callable[[str, tuple[str, ...]], Route]]:
    """Jev over EVE_JEV_ROUTE (2 s timeout), made once a process, or None (no route: every search stays on
    perplexity)."""
    from . import jev

    env = os.environ if environ is None else environ
    try:
        url, key, model = jev.route_config(env)
    except Exception:  # noqa: BLE001
        return None
    with _JEV_LOCK:
        cached = _JEV.get("router")
        if cached is None or _JEV.get("route") != (url, model):
            cached = make_router(jev.make_predict(url, key, model, timeout_s=JEV_TIMEOUT_S), time.perf_counter)
            _JEV.update(router=cached, route=(url, model))
        return cached


def mode(environ: Optional[Mapping[str, str]] = None) -> str:
    """EVE_SEARCH_ROUTER = auto (the default: on when SHIPPED) | on (on before the fit, for a live trial) |
    off (websearch's engine and the TinyFish reader, exactly as before the router)."""
    env = os.environ if environ is None else environ
    raw = (env.get("EVE_SEARCH_ROUTER") or "auto").strip().lower()
    if raw in ("1", "true", "yes"):
        raw = "on"
    if raw in ("0", "false", "no"):
        raw = "off"
    if raw not in MODES:
        log.warning("search router: EVE_SEARCH_ROUTER=%r is not one of %s; auto", raw, MODES)
        raw = "auto"
    return raw


def available(environ: Optional[Mapping[str, str]] = None, engine: str = "") -> tuple[str, ...]:
    """The providers routing may use, perplexity first, or () when routing is off. On only when the mode allows it,
    a Jev route is configured, and perplexity runs through OpenRouter (``engine`` is websearch's; every provider's
    answer model is on OpenRouter too, so without it nothing here can answer); then TinyFish with
    TINYFISH_API_KEY and Firecrawl with FIRECRAWL_API_KEY. Perplexity alone is not routing: ()."""
    from . import jev

    env = os.environ if environ is None else environ
    m = mode(env)
    if m == "off" or (m == "auto" and not SHIPPED):
        return ()
    if engine not in websearch.OPENROUTER_ENGINES or not (env.get("OPENROUTER_API_KEY") or "").strip():
        return ()
    try:
        jev.route_config(env)
    except Exception:  # noqa: BLE001
        return ()
    found = (PERPLEXITY,) + ((TINYFISH,) if tinyfish.api_key(env) else ()) + (
        (FIRECRAWL,) if firecrawl.api_key(env) else ())
    return found if len(found) > 1 else ()


# ---- the one routed entry point -----------------------------------------------------------------------------

@dataclass(frozen=True)
class Paths:
    """Each provider's calls, as seams for the tests; None is the real call, resolved when used."""

    perplexity: Optional[Callable[[str, str, str], dict[str, Any]]] = None   # (query, system, feature) -> result
    read: Optional[Callable[..., dict[str, Any]]] = None                     # web_reader.search_and_read
    firecrawl_search: Optional[Callable[..., dict[str, Any]]] = None         # firecrawl.search
    run_tool: Optional[Callable[[dict[str, Any]], dict[str, Any]]] = None    # firecrawl.run_tool
    ask: Optional[Callable[[dict[str, Any]], dict[str, Any]]] = None         # watch.openrouter


NOT_FOUND = "NOT FOUND"
# The web reader needs to know when a many-source search did not answer the task, so Codex can take it.
READER_SYSTEM = ("You are the answer step of eve's web search: eve, the owner's desk-robot assistant, searched "
                 "the web for the owner's request. Using only the web results provided, answer the request in at "
                 "most four plain sentences with the concrete facts (numbers, names, dates) and say which source "
                 "each comes from by its title. If the results do not answer it, reply with exactly: "
                 f"{NOT_FOUND}. No markdown, no lists.")
_LINK = re.compile(r"(?:https?://|www\.)\S+", re.I)


def keep_read_links(text: str, read: set[str]) -> str:
    """A link in an answer's own words survives only when eve read it (a page it fetched, a source the search
    cited, a link a data tool returned); any other becomes "(link removed)" (WatchLink.lean, the watcher's lesson)."""
    return " ".join(_LINK.sub(lambda m: m.group(0) if m.group(0).rstrip(".,;:!?") in read else "(link removed)",
                              text).split())


def _usage(payloads: list[dict[str, Any]]) -> dict[str, Any]:
    """The answer model's usage over its calls, websearch.parse's shape; ``cost`` only when every call reported one."""
    tin = tout = 0
    costs: list[float] = []
    for p in payloads:
        u = p.get("usage") if isinstance(p, dict) and isinstance(p.get("usage"), dict) else {}
        tin += int(u.get("prompt_tokens") or 0)
        tout += int(u.get("completion_tokens") or 0)
        if isinstance(u.get("cost"), (int, float)) and not isinstance(u.get("cost"), bool):
            costs.append(float(u["cost"]))
    out: dict[str, Any] = {"in": tin, "out": tout}
    if payloads and len(costs) == len(payloads):
        out["cost"] = round(sum(costs), 6)
    return out


def _result(answer: str, pages: list[dict[str, Any]], used: list[str], payloads: list[dict[str, Any]],
            extra_usd: Optional[float] = 0.0) -> dict[str, Any]:
    """websearch's result shape from a page-reading provider: the pages the answer used become its sources.
    ``cost_usd`` is the answer model's reported cost plus ``extra_usd`` (a Firecrawl route's credits), or None
    when either is unknown: an answer model that reported no cost, or credits with no price (``extra_usd`` None).
    None is "unpriced", as spend.py records it; a partial figure that looks whole is never returned."""
    by_url = {p["url"]: p for p in pages}
    sources = [{"title": " ".join(str(by_url[u].get("title") or "").split())[:120], "url": u[:500],
                "snippet": " ".join(str(by_url[u].get("markdown") or "").split())[:websearch.MAX_SNIPPET_CHARS]}
               for u in used if u in by_url][:websearch.MAX_SOURCES]
    usage = _usage(payloads)
    cost = usage.get("cost")
    return {"ok": True, "answer": answer[:websearch.MAX_ANSWER_CHARS], "sources": sources, "usage": usage,
            "cost_usd": round(cost + extra_usd, 4) if cost is not None and extra_usd is not None else None}


def via(provider: str, query: str, config: websearch.SearchConfig, *, reader: bool = False,
        paths: Paths = Paths(), feature: str = spend.SEARCH, cancelled: Callable[[], bool] = lambda: False,
        progress: Callable[[str], None] = lambda text: None, key: str = "", opener: Any = None,
        clock: Callable[[], float] = time.perf_counter) -> dict[str, Any]:
    """One provider, no fallback: websearch's result shape, or {"ok": False, "reason"}. Raises only what the
    provider's own code raises past watch.FetchError (``answer`` catches everything)."""
    from . import web_reader

    ask = paths.ask or watch.openrouter
    if provider == TINYFISH:
        try:
            found = (paths.read or web_reader.search_and_read)(query)
        except watch.FetchError as e:
            return {"ok": False, "reason": e.reason if e.reason.startswith("TinyFish") else f"TinyFish: {e.reason}"}
        pages = found.get("pages") or []
        spend.record("tinyfish", "search+fetch", feature, 0.0, note=f"{len(pages)} pages, free")
        if cancelled():
            return {"ok": False, "reason": "stopped"}
        if not pages:
            return {"ok": False, "reason": "TinyFish found no readable pages"}
        text, used, payload = web_reader.answer_parts(query, pages, ask=ask)
        spend.record_chat_completion(feature, payload, model=web_reader.MODEL)
        if not text:
            return {"ok": False, "reason": f"the {len(pages)} pages did not answer it"}
        return _result(text, pages, used, [payload])
    if provider == FIRECRAWL:
        return _firecrawl(query, paths=paths, ask=ask, feature=feature, cancelled=cancelled, progress=progress)
    # perplexity: today's websearch code, with the reader's answered-or-not rule when the reader asks
    system = READER_SYSTEM if reader else websearch.SYSTEM
    if paths.perplexity is not None:
        out = paths.perplexity(query, system, feature)
    else:
        # the reader needs an answer to ITS question or NOT_FOUND; Monid's raw Google snippets are neither, so the
        # reader never pays for them (review, 2026-10-08)
        out = websearch.engine_search(query, config, key=key, opener=opener, clock=clock, system=system,
                                      feature=feature, monid_ok=not reader)
    if reader and out.get("ok") and out.get("answer", "").strip().upper().startswith(NOT_FOUND):
        return {"ok": False, "reason": "the search did not answer it"}
    if out.get("ok"):
        out = {**out, "answer": keep_read_links(out.get("answer", ""), {s["url"] for s in out.get("sources") or []})}
    return out


def _firecrawl(query: str, *, paths: Paths, ask: Callable[[dict[str, Any]], dict[str, Any]], feature: str,
               cancelled: Callable[[], bool], progress: Callable[[str], None]) -> dict[str, Any]:
    """Firecrawl search with Alexandria's tools; the model answers from the pages or picks ONE tool, which eve
    runs and the model answers from. A tool that fails (or needs terms eve never accepts) leaves the pages."""
    from . import web_reader

    price = firecrawl.usd_per_credit()
    try:
        found = (paths.firecrawl_search or firecrawl.search)(query, tools=True)
    except watch.FetchError as e:
        return {"ok": False, "reason": e.reason if e.reason.startswith("Firecrawl") else f"Firecrawl: {e.reason}"}
    firecrawl.meter("search", found.get("credits"), feature, price)
    credits: Optional[float] = _credits(found)          # None: Firecrawl did not say what the call cost
    pages, tools = found.get("pages") or [], found.get("tools") or []
    if cancelled():
        return {"ok": False, "reason": "stopped"}
    if not pages and not tools:
        return {"ok": False, "reason": "Firecrawl found no readable pages"}
    text, used, payload = web_reader.answer_parts(query, pages, ask=ask, tools=tools)
    spend.record_chat_completion(feature, payload, model=web_reader.MODEL)
    payloads = [payload]
    call = firecrawl.pick_tool(payload, tools) if tools and not text else None
    if call is not None and not cancelled():
        name = call.pop("name")
        log.info("search router: Alexandria %s/%s", call["provider"], call["capability"])
        progress(f"Checking {name}…")
        try:
            ran: Optional[dict[str, Any]] = (paths.run_tool or firecrawl.run_tool)(call)
        except watch.FetchError as e:
            log.info("search router: %s; the pages answer instead", e.reason)
            ran = None
        if ran is not None:
            firecrawl.meter(f"alexandria {call['provider']}", ran.get("credits"), feature, price)
            more = _credits(ran)
            credits = None if credits is None or more is None else credits + more
        if cancelled():
            return {"ok": False, "reason": "stopped"}
        result = {"name": name, "data": ran["data"]} if ran is not None else None
        if result is not None or pages:
            text, used, payload = web_reader.answer_parts(query, pages, ask=ask, tool_result=result)
            spend.record_chat_completion(feature, payload, model=web_reader.MODEL)
            payloads.append(payload)
    if not text:
        return {"ok": False, "reason": f"the {len(pages)} pages did not answer it"}
    # Credits with no price on this plan (EVE_FIRECRAWL_USD unset), or credits Firecrawl did not report:
    # the route's cost is unknown, so cost_usd is None rather than the answer model's share passed off as all of it.
    extra = None if credits is None or (price is None and credits) else credits * (price or 0.0)
    return _result(text, pages, used, payloads, extra_usd=extra)


def _credits(reply: dict[str, Any]) -> Optional[float]:
    v = reply.get("credits")
    return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def bounded(work: Callable[[Callable[[], bool]], dict[str, Any]], budget: float) -> dict[str, Any]:
    """``work(abandoned)`` in a worker thread, waited on for ``budget`` seconds at most: its result, or
    {"ok": False, "reason"} once the budget is spent. An outer timeout, not the provider's per-call HTTP timeouts
    (a search, a fetch, a data tool and two model calls add up past any one of them): the worker is left to
    finish on its own, ``abandoned()`` turns True so it stops at its next step, and its result is dropped."""
    box: dict[str, Any] = {}
    done, gone = threading.Event(), threading.Event()

    def run() -> None:
        try:
            box["out"] = work(gone.is_set)
        except Exception as e:  # noqa: BLE001 — anything at all: the next provider
            box["out"] = {"ok": False, "reason": f"{type(e).__name__}"}
        finally:
            done.set()

    # The caller's context goes with the work: the spend rows a chief card's search writes keep its card id
    # (spend.job; a bare Thread starts with an empty context, 2026-09-29).
    threading.Thread(target=contextvars.copy_context().run, args=(run,), name="search-router", daemon=True).start()
    if not done.wait(max(0.0, budget)):
        gone.set()
        return {"ok": False, "reason": f"no answer within {budget:g} s"}
    return box.get("out") or {"ok": False, "reason": "no answer"}


def answer(query: str, config: websearch.SearchConfig, *, reader: bool = False, paths: Paths = Paths(),
           route: Optional[Callable[[str, tuple[str, ...]], Route]] = None, feature: str = spend.SEARCH,
           cancelled: Callable[[], bool] = lambda: False, progress: Callable[[str], None] = lambda text: None,
           tried: Callable[[str], None] = lambda provider: None, key: str = "", opener: Any = None,
           clock: Callable[[], float] = time.perf_counter) -> dict[str, Any]:
    """THE routed search, for both surfaces: websearch's result shape ({"ok", "answer", "sources", "usage", "ms",
    "cost_usd"}, cost_usd None when unpriced) or {"ok": False, "reason"}. Never raises.

    ``config.providers`` empty (routing off): the brain gets websearch's engine and the web reader (``reader``)
    gets TinyFish, each exactly as before the router. Otherwise Jev (``route``, default jev_router) picks among
    ``config.providers``, and a routed provider that fails, finds nothing, does not answer within its budget
    (``config.routed_secs`` for the brain, at least READER_ROUTED_SECS for the reader; ``bounded``) or would pass
    today's Firecrawl cap is followed by one perplexity search, the safe default, on its own timeout. Logs one
    line per routed search: the provider, the Choice's probability and Jev's milliseconds, never the query.
    ``tried`` hears each provider as it starts (the reader's status)."""
    t0 = clock()
    providers = tuple(config.providers)
    if not providers:
        order = [TINYFISH] if reader else [PERPLEXITY]
    else:
        router = route or jev_router()
        try:
            r = router(query, providers) if router is not None else Route(PERPLEXITY, error="no Jev route")
        except Exception as e:  # noqa: BLE001 — a router that fails is a perplexity search
            r = Route(PERPLEXITY, error=f"{type(e).__name__}")
        if r.provider not in providers:
            r = Route(PERPLEXITY, r.p, r.ms, r.error or f"{r.provider} is not available")
        if r.provider == FIRECRAWL and firecrawl.left() <= 0:
            r = Route(PERPLEXITY, r.p, r.ms, f"today's {firecrawl.per_day()} Firecrawl calls are used up")
        log.info("search router: %s (p=%.2f, jev %.0f ms%s)", r.provider, r.p, r.ms,
                 f", {r.error}" if r.error else "")
        order = [r.provider] + ([PERPLEXITY] if r.provider != PERPLEXITY else [])
    budget = max(config.routed_secs, READER_ROUTED_SECS) if reader else config.routed_secs
    out: dict[str, Any] = {"ok": False, "reason": "no provider ran"}
    for provider in order:
        tried(provider)

        def attempt(abandoned: Callable[[], bool], provider: str = provider) -> dict[str, Any]:
            return via(provider, query, config, reader=reader, paths=paths, feature=feature,
                       cancelled=lambda: cancelled() or abandoned(), progress=progress, key=key, opener=opener,
                       clock=clock)

        try:
            if providers and provider != PERPLEXITY:
                out = bounded(attempt, budget)          # a routed provider: never past its budget
            else:
                out = attempt(lambda: False)            # perplexity, or routing off: its own timeouts, as before
        except Exception as e:  # noqa: BLE001 — anything at all: the next provider, or the caller's fallback
            out = {"ok": False, "reason": f"{provider}: {type(e).__name__}"}
        if out.get("ok") or out.get("reason") == "stopped" or cancelled():
            break
        if len(order) > 1:
            log.info("search router: %s did not answer (%s); perplexity takes it", provider, out.get("reason"))
    if out.get("ok") and "ms" not in out:
        out["ms"] = round((clock() - t0) * 1000.0)
    return out
