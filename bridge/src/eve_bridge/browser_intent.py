"""What a browser request asks for, as one outcome: search, open a site, open a result, or none of these.

Two halves, split the way LayaBrowse splits them (github.com/cklxx/LayaBrowse, goal_engine.py):

* Code reads the literal arguments and nothing else: the site (from the table below, after the site names speech
  recognition splits are joined), the query (the words spoken after "search for" / "search <site> for") and a
  result number ("the first result", "the 3rd video"). Code never guesses a kind.
* Laya picks only the KIND, from that finite set, using the browser-trained checkpoint cklxx/laya-browser (v10s,
  pinned revision) through laya-mlx, on this Mac. It is asked the way LayaBrowse asks its outcome question: one
  Choice whose goal is the request, whose rules say "the outcome, not the next action", and whose options are the
  fixed outcome descriptions plus "unsupported". Its answer passes the same schema check and the same 0.50 / 0.10
  gate (top probability, margin over the runner-up) before anything uses it.

``expected_outcome`` returns None — an abstention — when the switch is off, laya-mlx is missing, the checkpoint is
not cached or fails to load, the answer is malformed or under the gate, or Laya's kind cannot be carried by the
literal arguments (a search with no query, a site that was never named, a result with no number). A "none"
outcome is an answer: the request is not one of these three.

EVE_BROWSER_INTENT=laya|off (default laya). Nothing leaves the Mac; Jev is not asked here.
"""

from __future__ import annotations

import collections
import logging
import math
import os
import re
import threading
import time
from dataclasses import dataclass, field
from typing import Any, Mapping, Optional
from urllib.parse import quote_plus

log = logging.getLogger(__name__)

# ---- the site table ----------------------------------------------------------------------------------------
#
# Hosts, homes, search URLs and query keys for GitHub, YouTube, Wikipedia, eBay and Google are LayaBrowse's
# (sites/*.json, MIT, Copyright (c) 2026 Aryan Bhujade); the rest are each site's own public search address.


@dataclass(frozen=True)
class Site:
    name: str                       # the site's own spelling: "GitHub", "Stack Overflow"
    host: str
    home: str
    search_template: str = ""       # "{query}" is replaced by the URL-encoded query
    query_key: str = ""             # the query parameter in search_template
    split: tuple[str, ...] = ()     # how speech recognition splits the name ("git hub")
    ambiguous: tuple[str, ...] = () # split forms that are also ordinary English: joined only after a cue word

    def search_url(self, query: str) -> str:
        return self.search_template.replace("{query}", quote_plus(query)) if self.search_template else ""


SITES: tuple[Site, ...] = (
    Site("GitHub", "github.com", "https://github.com/",
         "https://github.com/search?q={query}&type=repositories", "q", split=("git hub",)),
    Site("YouTube", "youtube.com", "https://www.youtube.com/",
         "https://www.youtube.com/results?search_query={query}", "search_query", split=("you tube",)),
    Site("Wikipedia", "wikipedia.org", "https://en.wikipedia.org/wiki/Main_Page",
         "https://en.wikipedia.org/w/index.php?search={query}", "search", split=("wiki pedia", "wikki pedia")),
    Site("eBay", "ebay.com", "https://www.ebay.com/",
         "https://www.ebay.com/sch/i.html?_nkw={query}", "_nkw", split=("e bay", "e-bay")),
    Site("Google", "google.com", "https://www.google.com/",
         "https://www.google.com/search?q={query}", "q"),
    Site("DuckDuckGo", "duckduckgo.com", "https://duckduckgo.com/",
         "https://duckduckgo.com/?q={query}", "q", split=("duck duck go", "duckduck go", "duck duckgo")),
    Site("Stack Overflow", "stackoverflow.com", "https://stackoverflow.com/",
         "https://stackoverflow.com/search?q={query}", "q", split=("stack overflow",),
         ambiguous=("stack overflow",)),
    Site("Reddit", "reddit.com", "https://www.reddit.com/",
         "https://www.reddit.com/search/?q={query}", "q", split=("red dit",)),
    Site("LinkedIn", "linkedin.com", "https://www.linkedin.com/",
         "https://www.linkedin.com/search/results/all/?keywords={query}", "keywords", split=("linked in",),
         ambiguous=("linked in",)),
    Site("Netflix", "netflix.com", "https://www.netflix.com/",
         "https://www.netflix.com/search?q={query}", "q", split=("net flix",)),
    Site("Amazon", "amazon.com", "https://www.amazon.com/", "https://www.amazon.com/s?k={query}", "k"),
    # Two words in its own spelling: nothing to join, but "hacker news" still names the site.
    Site("Hacker News", "news.ycombinator.com", "https://news.ycombinator.com/",
         "https://hn.algolia.com/?q={query}", "q", split=("hacker news",), ambiguous=("hacker news",)),
)

# The word just before an ambiguous form that makes it a site: "open linked in", "on stack overflow". Without one
# ("the doc I linked in slack", "a stack overflow error") the words are left as they are.
SITE_CUES = {"open", "visit", "on", "to", "at", "launch", "check", "browse", "search", "onto", "via", "using", "into"}


def _spoken(form: str) -> str:
    return r"[\s-]+".join(re.escape(part) for part in re.split(r"[\s-]+", form))


_SPLITS = sorted(((form, site) for site in SITES for form in site.split), key=lambda fs: -len(fs[0]))
_SPLIT_RE = re.compile(r"(?<![\w.])(" + "|".join(_spoken(f) for f, _ in _SPLITS) + r")(?![\w])", re.IGNORECASE)
_BY_FORM = {" ".join(re.split(r"[\s-]+", f.casefold())): site for f, site in _SPLITS}
_AMBIGUOUS = {" ".join(f.split()) for site in SITES for f in site.ambiguous}
_NAMES = {key: site for site in SITES
          for key in {site.name.casefold(), site.name.casefold().replace(" ", ""), site.host.split(".")[0]}}


def _cued(text: str, start: int) -> bool:
    before = re.findall(r"[a-z']+", text[:start].casefold())
    return not before or before[-1] in SITE_CUES


def join_site_names(text: str) -> str:
    """The text with split site names joined in the site's own spelling ("git hub" → "GitHub"); nothing else
    changes. A split form that is also ordinary English is joined only after a cue word ("open linked in")."""
    if not text:
        return text

    def join(m: re.Match) -> str:
        form = " ".join(re.split(r"[\s-]+", m.group(0).casefold()))
        if form in _AMBIGUOUS and not _cued(text, m.start()):
            return m.group(0)
        return _BY_FORM[form].name
    return _SPLIT_RE.sub(join, text)


def site_for(name: str) -> Optional[Site]:
    """The site a name, a joined name or a split form names; None for anything else."""
    key = " ".join(re.split(r"[\s-]+", str(name or "").strip().casefold()))
    return _NAMES.get(key) or _NAMES.get(key.replace(" ", "")) or _BY_FORM.get(key)


_NAME_RE = re.compile(r"(?<![\w])(" + "|".join(re.escape(s.name) for s in sorted(SITES, key=lambda s: -len(s.name)))
                      + r"|" + "|".join(re.escape(s.name.replace(" ", "")) for s in SITES) + r")(?:\.com|\.org)?(?![\w])",
                      re.IGNORECASE)


def mentioned_site(text: str) -> Optional[Site]:
    """The first site the request names, after joining. An ambiguous two-word name ("stack overflow") counts only
    in the site's own capitals or after a cue word, so "a stack overflow error" names no site."""
    joined = join_site_names(text)
    for m in _NAME_RE.finditer(joined):
        site = site_for(m.group(1))
        if site is None:
            continue
        if site.name.casefold() in _AMBIGUOUS and " " in m.group(1) and m.group(1) != site.name \
                and not _cued(joined, m.start()):
            continue
        return site
    return None


# ---- literal arguments -------------------------------------------------------------------------------------

ORDINALS = {"first": 1, "top": 1, "second": 2, "third": 3, "fourth": 4, "fifth": 5, "sixth": 6, "seventh": 7,
            "eighth": 8, "ninth": 9, "tenth": 10, "last": None}
NUMBERS = {"one": 1, "two": 2, "three": 3, "four": 4, "five": 5, "six": 6, "seven": 7, "eight": 8, "nine": 9,
           "ten": 10}
RESULT_NOUN = r"(?:result|video|link|item|one|repo|repository|article|listing|product|hit|story|post|page|song|track)s?"
_ORDINAL_RE = re.compile(r"\b(" + "|".join(k for k in ORDINALS if k != "last") + r"|\d{1,2}(?:st|nd|rd|th))\s+"
                         r"(?:[a-z]+\s+)?" + RESULT_NOUN + r"\b", re.IGNORECASE)
_NUMBER_RE = re.compile(r"\b(?:result|video|link|item)\s+(?:number\s+)?(\d{1,2}|" + "|".join(NUMBERS) + r")\b",
                        re.IGNORECASE)
_SITE_ALT = "|".join(re.escape(s.name) for s in sorted(SITES, key=lambda s: -len(s.name)))
_SEARCH_SITE_FOR = re.compile(r"\bsearch\s+(?:on\s+|in\s+)?(?:" + _SITE_ALT + r")(?:\.com|\.org)?\s+for\s+(.+)",
                              re.IGNORECASE)
_SEARCH_FOR = re.compile(r"\b(?:search\s+(?:for|up)|look\s+up)\s+(.+)", re.IGNORECASE)
_QUERY_END = [
    re.compile(r"\s+(?:on|in|using|at|from|via)\s+(?:" + _SITE_ALT + r")(?:\.com|\.org)?\b.*$", re.IGNORECASE),
    re.compile(r"\s*,?\s*\b(?:and|then)\b\s+(?:then\s+)?(?:open|click|play|show|go|pick|select|watch|choose|read|tap)\b.*$",
               re.IGNORECASE),
    re.compile(r"\s+in\s+(?:a\s+)?new\s+tab\b.*$", re.IGNORECASE),
]


@dataclass(frozen=True)
class Arguments:
    site: Optional[Site]
    query: Optional[str]
    n: Optional[int]


def _query(text: str) -> Optional[str]:
    m = _SEARCH_SITE_FOR.search(text) or _SEARCH_FOR.search(text)
    if not m:
        return None
    query = m.group(1)
    for end in _QUERY_END:
        query = end.sub("", query)
    query = query.strip().strip(" .,!?;:")
    if len(query) >= 2 and query[0] == query[-1] and query[0] in "\"'":
        query = query[1:-1]
    query = " ".join(query.split()).strip(" .,!?;:\"'")
    return query or None


def _ordinal(text: str) -> Optional[int]:
    m = _ORDINAL_RE.search(text)
    if m:
        word = m.group(1).casefold()
        return ORDINALS[word] if word in ORDINALS else int(re.match(r"\d+", word).group(0))
    m = _NUMBER_RE.search(text)
    if m:
        word = m.group(1).casefold()
        return NUMBERS.get(word) or int(word)
    return None


def literal_arguments(request: str) -> Arguments:
    """Site, query and result number, read from the words alone. Each is None when the request does not say it."""
    text = join_site_names(" ".join(str(request or "").split()))
    n = _ordinal(text)
    return Arguments(mentioned_site(text), _query(text), n if n and n > 0 else None)


# ---- the outcome ---------------------------------------------------------------------------------------------

KINDS = ("search", "open_site", "open_result", "none")


@dataclass(frozen=True)
class RequestOutcome:
    kind: str                       # exactly one of KINDS
    site_host: Optional[str]
    query: Optional[str]
    n: Optional[int]
    confidence: float
    source: str                     # "laya" or "code"
    latency_ms: float = field(default=0.0, compare=False)


# LayaBrowse's own outcome descriptions and rules (goal_engine.objective_descriptions and the "objective"
# question, MIT, Copyright (c) 2026 Aryan Bhujade), so the checkpoint is asked in the shape it was trained on.
OPTIONS = {
    "open_site": "Open the requested website. No search or result opening.",
    "search": "Search for the requested topic and show the results.",
    "open_result": "Open a particular video, article, repository or item from search results.",
    "unsupported": "Not a command, ambiguous, or these outcomes omit part of the request",
}
OPTION_KIND = {"open_site": "open_site", "search": "search", "open_result": "open_result", "unsupported": "none"}
OPTION_KEY = {kind: key for key, kind in OPTION_KIND.items()}
RULES = ("Choose the outcome requested by the speaker, not the next action. "
         "Include ALL requested work, including opening a result. "
         "Unrelated speech, unsupported actions or missing details mean unsupported.")
GATE_TOP, GATE_MARGIN = 0.50, 0.10

BROWSER_MODEL = "cklxx/laya-browser"
BROWSER_REVISION = "8a7e625b481d292d830acf22a2feec81554575fa"
BROWSER_SUBFOLDER = "v10s"
# MLX keeps freed GPU buffers for reuse and, uncapped, grew past 1.8 GB in a mostly idle background process
# (LayaBrowse laya.py); 64 MB costs about a millisecond a decision.
MLX_CACHE_LIMIT_BYTES = 64 * 1024 * 1024

MODES = ("laya", "off")
_LATENCIES: collections.deque = collections.deque(maxlen=256)


def mode(environ: Optional[Mapping[str, str]] = None) -> str:
    env = os.environ if environ is None else environ
    value = (env.get("EVE_BROWSER_INTENT") or "laya").strip().casefold()
    return value if value in MODES else "off"


def recent_latencies() -> list[float]:
    """Milliseconds per Laya call, newest last (the last 256)."""
    return list(_LATENCIES)


class Abstain(Exception):
    pass


def validate_choice(answer: Any, options: Mapping[str, str]) -> tuple[str, float]:
    """(option, top probability) when the answer is a well-formed choice that clears the gate, else Abstain.
    Mirrors LayaBrowse goal_engine.validate_choice: schema integrity, then 0.50 top and 0.10 margin."""
    try:
        probabilities = answer["probabilities"]
        choice = answer["choice"]
        values = list(probabilities.values())
        valid = (choice in options and set(probabilities) == set(options)
                 and all(type(v) in {int, float} and math.isfinite(v) and 0 <= v <= 1 for v in values)
                 and abs(sum(values) - 1) < 0.02
                 and probabilities[choice] >= max(values) - 0.0002)
    except (KeyError, TypeError, ValueError, AttributeError):
        valid = False
    if not valid:
        raise Abstain("malformed answer")
    ordered = sorted(values, reverse=True)
    if ordered[0] < GATE_TOP or (len(ordered) > 1 and ordered[0] - ordered[1] < GATE_MARGIN):
        raise Abstain("under the gate")
    return choice, float(probabilities[choice])


def _cached_checkpoint() -> Optional[str]:
    """The pinned checkpoint's local snapshot, found without the network; None when it is not downloaded."""
    try:
        from huggingface_hub import snapshot_download

        # Only the v10s files: the repository's other checkpoints and assets are never downloaded, and without the
        # pattern huggingface_hub calls a v10s-only cache incomplete.
        return snapshot_download(BROWSER_MODEL, revision=BROWSER_REVISION, local_files_only=True,
                                 allow_patterns=[f"{BROWSER_SUBFOLDER}/*"])
    except Exception:  # noqa: BLE001 — not cached, or no huggingface_hub: abstain
        return None


class LayaKindEngine:
    """One process-local laya-mlx agent, loaded on first use and warmed once. A failed load is remembered, so a
    missing checkpoint costs one attempt, not one per request. Predictions are serialised under one lock."""

    def __init__(self) -> None:
        self._agent: Any = None
        self._failed = ""
        self._lock = threading.Lock()

    @property
    def loaded(self) -> bool:
        return self._agent is not None

    def warm(self) -> None:
        if self._agent is not None:
            return
        with self._lock:
            if self._agent is not None:
                return
            if self._failed:
                raise Abstain(self._failed)
            try:
                import laya_mlx
                import mlx.core as mx
            except Exception as exc:  # noqa: BLE001 — laya-mlx not installed: abstain, and keep abstaining
                self._failed = f"laya-mlx unavailable: {exc}"
                raise Abstain(self._failed) from exc
            path = _cached_checkpoint()
            if not path:
                # Never download from inside a request; the checkpoint is fetched once, by hand.
                self._failed = f"{BROWSER_MODEL}@{BROWSER_REVISION[:7]} is not cached"
                raise Abstain(self._failed)
            try:
                mx.set_cache_limit(MLX_CACHE_LIMIT_BYTES)
                self._agent = laya_mlx.load(path, subfolder=BROWSER_SUBFOLDER, revision=BROWSER_REVISION,
                                            dtype="float16", cache_prompts=True)
            except Exception as exc:  # noqa: BLE001 — a checkpoint that will not load abstains
                self._failed = f"load failed: {exc}"
                log.warning("browser intent: %s", self._failed)
                raise Abstain(self._failed) from exc

    def _prefix_fits(self, question: dict) -> bool:
        """The question and its options fit the checkpoint's head budget (LayaBrowse goal_engine._prefix_fits):
        refuse rather than let laya-mlx truncate silently."""
        from laya_mlx.common import render_options

        q = self._agent._to_internal(question)
        tok = self._agent.tok

        def length(s: str) -> int:
            return len(tok(s, add_special_tokens=False)["input_ids"])
        options = [length(" " + o.replace(tok.mask_token, " ")) for o in render_options(q)]
        instruction = length(f"{q['t']} question: {q['ins']}".replace(tok.mask_token, " "))
        return (all(n <= 48 for n in options)
                and sum(n + 1 for n in options) + max(16, instruction) <= self._agent.cfg["head_max_len"])

    def ask(self, goal: str, question: dict, state: dict) -> dict:
        self.warm()
        if not self._prefix_fits(question):
            raise Abstain("request exceeds the checkpoint's question budget")
        with self._lock:
            result = self._agent.predict(state, {"objective": question})
        return (result.get("answers") or {}).get("objective", {}) if isinstance(result, dict) else {}


_ENGINE: Optional[LayaKindEngine] = None
_ENGINE_LOCK = threading.Lock()


def shared_engine() -> LayaKindEngine:
    global _ENGINE
    with _ENGINE_LOCK:
        if _ENGINE is None:
            _ENGINE = LayaKindEngine()
        return _ENGINE


def offered(args: Arguments) -> dict[str, str]:
    """The options Laya is shown: only the kinds these arguments could carry, then "unsupported" — the way
    LayaBrowse's contract_options lets a spoken query veto opening the bare site and a spoken result number veto a
    plain search, so Laya never weighs an outcome that would drop part of the request."""
    kinds = [kind for kind in ("open_site", "search", "open_result")
             if kind == "open_result" and (args.n is not None or args.query) or _carried(kind, args)]
    return {**{OPTION_KEY[kind]: OPTIONS[OPTION_KEY[kind]] for kind in kinds}, "unsupported": OPTIONS["unsupported"]}


def _carried(kind: str, args: Arguments) -> bool:
    """Can the literal arguments carry this kind, with no part of the request dropped?"""
    if kind == "search":
        return bool(args.query) and args.n is None
    if kind == "open_site":
        return args.site is not None and not args.query and args.n is None
    if kind == "open_result":
        return args.n is not None
    return kind == "none"


def expected_outcome(request: str, *, engine: Any = None,
                     environ: Optional[Mapping[str, str]] = None) -> Optional[RequestOutcome]:
    """The outcome the request asks for, or None to abstain (module docstring). `engine` answers
    ``ask(goal, question, state) -> laya-mlx choice answer``; the default is the shared local Laya."""
    if mode(environ) != "laya":
        return None
    text = join_site_names(" ".join(str(request or "").split()))
    if not text:
        return None
    args = literal_arguments(text)
    if args.site is None and args.query is None and args.n is None:
        # Nothing to search for, no site and no result: none of the three can be carried out.
        return RequestOutcome("none", None, None, None, 1.0, "code")
    asker = engine if engine is not None else shared_engine()
    options = offered(args)
    question = {"type": "choice", "instructions": {"goal": text, "rules": RULES}, "criteria": options}
    # No page yet: the outcome depends on the words. Same keys as LayaBrowse's goal state.
    state = {"page": {"url": "about:blank", "title": "", "text": ""}, "recent_actions": []}
    started = time.perf_counter()
    try:
        answer = asker.ask(text, question, state)
        latency = (time.perf_counter() - started) * 1000
        _LATENCIES.append(latency)
        option, confidence = validate_choice(answer, options)
    except Abstain as exc:
        log.debug("browser intent abstains: %s", exc)
        return None
    except Exception as exc:  # noqa: BLE001 — any model failure is an abstention, never a crash in routing
        log.warning("browser intent: %s", exc)
        return None
    kind = OPTION_KIND[option]
    if not _carried(kind, args):
        return None
    if kind == "none":
        return RequestOutcome("none", None, None, None, round(confidence, 4), "laya", latency)
    return RequestOutcome(
        kind,
        args.site.host if args.site else None,
        args.query if kind in ("search", "open_result") else None,
        args.n if kind == "open_result" else None,
        round(confidence, 4),
        "laya",
        latency,
    )
