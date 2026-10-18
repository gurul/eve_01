"""Which of the owner's toolsets a Telegram turn starts with: Jev's judgment of the message, plus keywords.

Why (2026-10-05): a turn starts with only the toolsets its words name (apps_mcp.APP_WORDS) and an ``open_app`` tool
for the rest. A toolset the turn needs but did not start with costs a whole model round to open it, and worse, it
changes the tool list mid-turn, which busts the prompt cache: live 2026-10-05, a 13k-token turn whose tool list
changed read cached=0 on the next round. Keywords miss most of what the owner actually types ("whens my thing w
the advisor", "did anyone @ me"); Jev reads the job and answers in 148-266 ms (measured live
2026-10-05: "my next class and a maps link" -> calendar 0.93, maps 0.90).

The seven toolsets (the datasets' policy field is the definition):

* calendar, maps, gmail, drive, github: the direct apps (apps_mcp.py), opened per turn.
* share: sending a file from the Mac to another person by iMessage or email (share.py).
* workspace: the owner's Slack, Notion and Linear (composio_tools.py).

Jev is asked the way search_router.py asks it (docs.typesafe.ai: State, Noul; typed_ask.py's idiom):

* The state is the message, the owner's previous message (a follow-up's context: "and send it to ana" after "find
  my resume in drive" needs drive AND share), and one line saying what the text is. No tool manuals in the state.
* One absolute Noul per toolset, each a plain judgment of the job ("must the assistant read or change the owner's
  own calendar ..."), written in the owner's own words for things and naming no wrong thing (the owner's rule:
  never write a wrong *name* into a prompt; a name in a prompt is an option the model may fill a slot with).
* Code owns the rule: a toolset opens when its Noul clears its fitted gate (GATES), OR its keywords match
  (keyword_toolsets). An error, a timeout or a missing key is the empty set from Jev: keywords and open_app still
  apply, exactly as before the router.

Fitting (tools/toolset_eval.py, the house recipe of tools/route_eval.py --search): live Jev runs once over the
tuning set (toolset_tuning.json, 220 cases, writer A; answers cached in toolset_tuning_answers.json), and per
toolset the HIGHEST gate on GRID is kept such that keywords + Jev reach recall >= 0.98 on the tuning cases that
need it. Then live Jev runs ONCE over the blind holdout (toolset_holdout.json, 120 cases, writer B) and is scored
against the bar below, written before any holdout result existed. Nothing is refitted after the holdout.

PRE-REGISTERED BAR (written by the orchestrator 2026-10-05, before any holdout result existed), verbatim:
  (1) case-level full recall (every needed toolset opened) of keywords∪Jev >= 0.95;
  (2) strictly higher than keywords alone on the same metric;
  (3) mean unneeded toolsets opened per case <= 0.6;
  (4) Jev latency p95 <= 800 ms.
SHIPPED is True only if all four hold.

``EVE_TOOLSET_ROUTER`` = auto (the default: Jev on when SHIPPED) | on (Jev on before or despite the bar, for a
live trial) | off (keywords only). Jev is a third party: the owner's message and the previous one leave the Mac on
every Telegram turn while it is on.
"""

from __future__ import annotations

import hashlib
import json
import logging
import os
import re
import threading
import time
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Mapping, Optional

log = logging.getLogger(__name__)

Predict = Callable[[Any, dict[str, Any]], dict[str, Any]]

CALENDAR, MAPS, GMAIL, DRIVE, GITHUB = "calendar", "maps", "gmail", "drive", "github"
SHARE, WORKSPACE = "share", "workspace"
# 2026-10-05: an eighth toolset, ordering delivery, was removed with its integration (docs/toolset-router.md). The
# datasets and cached answers keep its labels and Nouls as history; the eval and the fit read only these.
TOOLSETS = (CALENDAR, MAPS, GMAIL, DRIVE, GITHUB, SHARE, WORKSPACE)
# Which toolsets are apps_mcp's apps is AppsBridge.apps at run time (the list grew to contacts on 2026-10-09); the
# other two, share and workspace, are telegram's own tools.
# Opened by their words alone, never asked of Jev (2026-10-09): Google Contacts is one small read tool, so a false
# open costs a few hundred tokens, and a Noul of its own would change the call the gates above were fitted on.
CONTACTS = "contacts"
KEYWORD_ONLY = (CONTACTS,)

# The toolsets every turn carries, outside the routed seven (2026-10-08, owner: "expose all the menu items natively
# as well"). They are never routed: each is a few small tools whose schemas are fixed, so carrying them on every turn
# keeps the cached prefix stable, while a routed toolset would need its own Noul in Jev's call (which changes the
# call the gates above were fitted on) and a refit. Each is lent only while this Mac has it (telegram._menu_tools):
#
# * menu: the / menu's commands that had no tool (menu_tools.py), each reaching its command's own handler;
# * events: concerts, games and shows by place and date on Ticketmaster (events.py), while TICKETMASTER_API_KEY is
#   set. Event questions reach it on every turn, so web_search is left for the rest of the web.
MENU, EVENTS = "menu", "events"
ALWAYS: dict[str, tuple[str, ...]] = {
    MENU: ("spend_report", "daily_rundown", "claude_relay", "codex_relay", "set_stealth", "open_apps",
           "pair_eve_link", "read_file"),
    EVENTS: ("events_search",),
}


def always_toolset(tool: str) -> str:
    """The always-carried toolset a tool belongs to, or ""."""
    return next((name for name, tools in ALWAYS.items() if tool in tools), "")
MODES = ("auto", "on", "off")
# Jev's whole budget at a turn's start. Live 2026-10-05: 148-266 ms; past this the turn starts on keywords alone,
# which is today's behaviour, so a slow Jev costs at most this much and never a wrong tool list.
TIMEOUT_S = 0.8
GRID = tuple(round(0.05 * i, 2) for i in range(1, 20))  # 0.05 .. 0.95
MIN_TUNING_RECALL = 0.98
GATES_FILE = Path(__file__).resolve().parents[2] / "tests" / "fixtures" / "routes" / "toolset_gates.json"

# ---- the question ---------------------------------------------------------------------------------------------

ABOUT = ("A message the owner texted their personal assistant. When a previous message is given, the new one may "
         "be a follow-up to it: judge the job the new message asks for, reading the previous one only as context.")

# One Noul per toolset: does doing what the owner asks need this? Each is the job, in the words the owner uses.
INSTRUCTIONS: dict[str, str] = {
    CALENDAR: (
        "To do what the owner asks, must the assistant read or change the owner's own calendar: their events, "
        "classes, lectures, exams, meetings, appointments, free or busy time, invites, what is next or what their "
        "day or week holds? Yes too when the request points at one of their events by its time or name (\"my 3pm\", "
        "\"my next class\") and needs when or where it is, or when the request asks to put something on their "
        "calendar. No for general questions about dates or how calendars work."),
    MAPS: (
        "To do what the owner asks, must the assistant look up places or getting around: a place's address or map "
        "link, directions, travel time, the route, traffic, places nearby to go to, or the weather or forecast "
        "somewhere? No for a business's opening hours, news, or general facts."),
    GMAIL: (
        "To do what the owner asks, must the assistant read the owner's own email: find, read, count or summarize "
        "messages in their inbox, what someone emailed or sent them, or a receipt, code, link or confirmation that "
        "came by email? No when the request only sends something, and no for whether an email service is working."),
    DRIVE: (
        "To do what the owner asks, must the assistant find, open, read or create a document, spreadsheet, slide "
        "deck or PDF stored in the owner's cloud drive, or save something there? No for a file only on the "
        "computer's desktop or downloads folder."),
    GITHUB: (
        "To do what the owner asks, must the assistant read or change the owner's code projects on GitHub: "
        "repositories, issues, pull requests, commits, branches, or build and CI status? No for general "
        "programming questions or advice."),
    SHARE: (
        "To do what the owner asks, must the assistant send a file (a document, deck, PDF, photo, screenshot or "
        "other file) to another person by text message or email? No when the file goes to the owner themself, "
        "and no for a plain message with no file."),
    WORKSPACE: (
        "To do what the owner asks, must the assistant read or write in the owner's Slack, Notion or Linear: "
        "messages, channels, mentions, threads, pages, tickets or sprints? No for opening one of those apps on the "
        "computer screen, or for comparing such products."),
}


def questions() -> dict[str, Any]:
    """One absolute Noul per toolset, the same on every Mac whichever toolsets are up (so the fitted gates hold)."""
    return {name: {"type": "noul", "instructions": INSTRUCTIONS[name]} for name in TOOLSETS}


def state(text: str, previous: str = "") -> dict[str, str]:
    return {"about": ABOUT, "previous": " ".join(str(previous or "").split())[:500],
            "request": " ".join(str(text or "").split())[:500]}


@dataclass(frozen=True)
class ToolsetAnswer:
    nouls: Mapping[str, float] = field(default_factory=dict)    # toolset -> Jev's Noul, 0.0 when absent
    ms: float = 0.0
    error: str = ""

    def to_json(self) -> dict[str, Any]:
        return {"nouls": {k: round(float(v), 4) for k, v in self.nouls.items()}, "ms": round(self.ms, 1),
                "error": self.error}

    @staticmethod
    def from_json(raw: Mapping[str, Any]) -> "ToolsetAnswer":
        return ToolsetAnswer({k: float(v) for k, v in (raw.get("nouls") or {}).items()}, float(raw.get("ms") or 0.0),
                             str(raw.get("error") or ""))


@dataclass(frozen=True)
class Route:
    toolsets: frozenset[str] = frozenset()
    ms: float = 0.0                  # Jev's time
    error: str = ""


def ask(predict: Predict, text: str, previous: str, clock: Callable[[], float]) -> ToolsetAnswer:
    """Jev's Nouls for one message. A failure is an answer with ``error`` set and no Nouls: it opens nothing."""
    t0 = clock()
    try:
        answers = predict(state(text, previous), questions()).get("answers") or {}
    except Exception as e:  # noqa: BLE001 — a model that fails abstains: keywords and open_app still apply
        return ToolsetAnswer({}, (clock() - t0) * 1000.0, f"{type(e).__name__}: {e}"[:160])

    def noul(key: str) -> float:
        v = (answers.get(key) or {}).get("noul") if isinstance(answers.get(key), dict) else None
        return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else 0.0

    return ToolsetAnswer({name: noul(name) for name in TOOLSETS}, (clock() - t0) * 1000.0)


def decide(a: ToolsetAnswer, gates: Mapping[str, float], budget_ms: float = float("inf")) -> frozenset[str]:
    """The toolsets whose Noul clears its gate. An error, or an answer slower than ``budget_ms`` (the turn would
    not have waited for it), opens nothing."""
    if a.error or a.ms > budget_ms:
        return frozenset()
    return frozenset(name for name in TOOLSETS if a.nouls.get(name, 0.0) >= gates.get(name, 1.01))


# ---- keywords ---------------------------------------------------------------------------------------------------

# share and workspace have no apps_mcp.APP_WORDS; these are theirs, written from the tuning set's positives and its
# hard negatives ("send me the pdf", "email it to me", "open slack on my mac", "notion vs obsidian?"). They are a
# net under Jev, not the router: a miss here is Jev's to catch.
_SHARE_VERB = r"(?:send|text|i-?message|e-?mail|shoot|forward)"
_SHARE_THING = (r"(?:deck|pdf|file|files|docs?|slides|resume|cover letter|screenshot|pic|pics|photos?|zip|sheet|"
                r"report|itinerary|lease|contract|attachment|it|that|this|them)")
SHARE_WORDS = re.compile(rf"\b{_SHARE_VERB}\b(?:\s+\S+){{0,4}}?\s+(?:the |my |a |that |this )?{_SHARE_THING}\b",
                         re.I)
TO_SELF = re.compile(rf"\b{_SHARE_VERB} (?:it |that |this |them )?(?:to )?me\b|\bto me\b|\bme (?:here|too)\b", re.I)
WORKSPACE_WORDS = re.compile(
    r"(?:\b(?<!open )(?<!launch )(?<!quit )(?<!close )slack(?:ed|ing)?\b(?! (?:app|on my mac))|"
    r"\bnotion\b(?! vs\b| or\b)|\blinear\b(?! (?:algebra|regression|time|function))|\btickets?\b|\bchannels?\b|"
    r"#[a-z][\w-]*|@\s?me\b|\bdms?\b|\bthreads?\b|\bsprint\b|\bstandup thread\b|"
    # the owner's work Gmail and calendar live in Composio (composio_tools.OTHER_ACCOUNT_TOOLKITS, 2026-10-05)
    r"\bwork (?:e-?mails?|mail|inbox|gmail|calendar|cal)\b|\bera (?:e-?mail|mail|inbox|calendar)\b)",
    re.I)
OWN_WORDS = {WORKSPACE: WORKSPACE_WORDS}

# A short follow-up that points back ("and send it to ana", "put that on my cal"): its keywords alone cannot say
# what "it" is. Chosen on the tuning set only (tools/toolset_eval.py --fit prints both): the previous message's
# keywords are added only for such a follow-up, so "lights blue" then "dimmer" stays empty.
FOLLOW_UP = re.compile(r"^(?:and|also|ok|okay|now|then|wait|actually|so|cool|fine)\b|\b(?:it|that|those|them|"
                       r"there|the first one|same|instead|too)\b", re.I)


def _words(text: str) -> set[str]:
    from . import apps_mcp  # imported late: apps_mcp pulls httpx, and the router must load without it

    found = {app for app in apps_mcp.APP_WORDS if apps_mcp.names_app(app, text)}
    found |= {name for name, rx in OWN_WORDS.items() if rx.search(text)}
    if SHARE_WORDS.search(text) and not TO_SELF.search(text):
        found.add(SHARE)
    return found & (set(TOOLSETS) | set(KEYWORD_ONLY))


def keyword_toolsets(text: str, previous: str = "") -> set[str]:
    """The toolsets the message's words name; for a short follow-up that points back, the previous message's too."""
    text = " ".join(str(text or "").split())
    found = _words(text)
    if previous and len(text.split()) <= 12 and FOLLOW_UP.search(text):
        found |= _words(" ".join(str(previous).split()))
    return found


# ---- the fit ----------------------------------------------------------------------------------------------------

def recall(said: list[frozenset[str] | set[str]], needed: list[frozenset[str] | set[str]], toolset: str) -> float:
    want = [i for i, n in enumerate(needed) if toolset in n]
    return sum(1 for i in want if toolset in said[i]) / len(want) if want else 1.0


def fit(answers: list[ToolsetAnswer], keywords: list[set[str]], needed: list[set[str]],
        budget_ms: float = TIMEOUT_S * 1000.0) -> dict[str, float]:
    """Per toolset, the HIGHEST gate on GRID at which keywords + Jev open it for >= MIN_TUNING_RECALL of the cases
    that need it (the highest gate is the fewest unneeded opens). The toolsets are independent: one's gate never
    changes another's opens. No gate reaching the recall: the lowest on GRID, and the eval says so."""
    gates: dict[str, float] = {}
    for name in TOOLSETS:
        chosen = GRID[0]
        for g in sorted(GRID, reverse=True):
            said = [kw | (decide(a, {name: g}, budget_ms) & {name}) for a, kw in zip(answers, keywords, strict=True)]
            if recall(said, needed, name) >= MIN_TUNING_RECALL:
                chosen = g
                break
        gates[name] = chosen
    return gates


# Fitted 2026-10-05 (tools/toolset_eval.py --fit, live Jev once over toolset_tuning.json, 220 cases; answers cached
# in toolset_tuning_answers.json). Mirrored in toolset_gates.json, which the eval writes and a test checks. On the
# tuning set (seen data, not evidence): keywords alone fully covered 163 of 220 cases, keywords + Jev 219 (the one
# miss: "when is cse 446 this week", calendar 0.33), mean unneeded toolsets 0.17 -> 0.32; Jev p50 177 ms, p95
# 312 ms. gmail's gate is the lowest because its Noul is shy on mail named without the word ("summarize the thread
# w ana about the trip" 0.22, "what did the ta say about my regrade" 0.25); share's is the top of GRID because its
# keywords already catch every tuning case, so Jev adds share only when it is near certain.
GATES: dict[str, float] = {
    CALENDAR: 0.55, MAPS: 0.55, GMAIL: 0.2, DRIVE: 0.6, GITHUB: 0.8, SHARE: 0.95, WORKSPACE: 0.8,
}

# PRE-REGISTERED BAR (above, verbatim in the module docstring and docs/toolset-router.md): scored once on the blind
# toolset_holdout.json by tools/toolset_eval.py --holdout. SHIPPED is True only when all four hold.
#
# Scored 2026-10-05 (live Jev once, 120 cases, answers kept in toolset_holdout_answers.json; nothing tuned after):
# (1) full recall 118/120 = 98.3% (>= 95%) pass; (2) keywords alone 92/120 = 76.7%, strictly lower, pass; (3) mean
# unneeded toolsets 0.333 (<= 0.6) pass; (4) Jev p95 286 ms, p50 172 ms, 0 errors, 0 over budget (<= 800) pass.
# Missed toolsets 30 -> 2 (each an open_app round). The two misses, verbatim:
#   "how long a drive to the restaurant for dinner w sam tonight": needed calendar+maps, calendar 0.51 < 0.55
#   "send my mom the boarding pass on my desktop and check traffic to sea-tac": needed maps+share, share 0.94 < 0.95
#   (the share keywords have no "boarding pass") and gmail 0.35 opened unneeded.
#
# Re-scored 2026-10-05 after ordering delivery was removed (no Jev calls: the cached answers, its labels and Nouls
# dropped when read; the other gates unchanged): tuning 219/220 with keywords + Jev, mean unneeded 0.318; holdout
# (1) 118/120 = 98.3% pass; (2) keywords alone 98/120 = 81.7%, strictly lower, pass; (3) mean unneeded 0.317 pass;
# (4) p95 286 ms pass (the cached latencies). Jev was asked one more Noul in the same call when those answers were
# cached, so a fresh blind set should confirm on the next refit.
SHIPPED = True


def load_gates(path: Path = GATES_FILE) -> dict[str, float]:
    """toolset_gates.json's gates, or GATES when the file is missing or unreadable."""
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))["gates"]
        return {name: float(raw[name]) for name in TOOLSETS}
    except Exception:  # noqa: BLE001
        return dict(GATES)


# ---- the router -------------------------------------------------------------------------------------------------

def router_from(predict: Predict, clock: Callable[[], float] = time.perf_counter,
                gates: Optional[Mapping[str, float]] = None) -> Callable[[str, str], Route]:
    """``route(text, previous) -> Route``: Jev's Nouls under the gates (the module's GATES when none are given,
    read at call time so a refit applies)."""
    def route(text: str, previous: str = "") -> Route:
        a = ask(predict, text, previous, clock)
        return Route(decide(a, gates or GATES), a.ms, a.error)
    return route


def mode(environ: Optional[Mapping[str, str]] = None) -> str:
    """EVE_TOOLSET_ROUTER = auto (the default: on when SHIPPED) | on (a live trial before or despite the bar) |
    off (keywords only, as before the router)."""
    env = os.environ if environ is None else environ
    raw = (env.get("EVE_TOOLSET_ROUTER") or "auto").strip().lower()
    if raw in ("1", "true", "yes"):
        raw = "on"
    if raw in ("0", "false", "no"):
        raw = "off"
    if raw not in MODES:
        log.warning("toolset router: EVE_TOOLSET_ROUTER=%r is not one of %s; auto", raw, MODES)
        raw = "auto"
    return raw


_JEV: dict[str, Any] = {}
_JEV_LOCK = threading.Lock()


def make_router(environ: Optional[Mapping[str, str]] = None) -> Optional[Callable[[str, str], Route]]:
    """Jev over EVE_JEV_ROUTE (TIMEOUT_S), made once a process, or None: the mode is off (or auto while not
    SHIPPED), or no Jev route is configured. None means keywords only."""
    from . import jev

    env = os.environ if environ is None else environ
    m = mode(env)
    if m == "off" or (m == "auto" and not SHIPPED):
        return None
    try:
        url, key, model = jev.route_config(env)
    except Exception:  # noqa: BLE001
        return None
    # the key is part of the route (a rotated key makes a new router), kept only as a fingerprint
    route = (url, model, hashlib.sha256(str(key).encode()).hexdigest()[:16])
    with _JEV_LOCK:
        cached = _JEV.get("router")
        if cached is None or _JEV.get("route") != route:
            cached = router_from(jev.make_predict(url, key, model, timeout_s=TIMEOUT_S), time.perf_counter)
            _JEV.update(router=cached, route=route)
        return cached


@dataclass(frozen=True)
class TurnToolsets:
    """One turn's toolsets and where each came from, for the turn's single log line (telegram._think)."""
    keywords: frozenset[str] = frozenset()
    jev: frozenset[str] = frozenset()
    ms: float = 0.0                  # Jev's time; 0 when it was not asked
    asked: bool = False              # a router was there to ask
    error: str = ""                  # Jev's error, or "timeout" when it did not answer within the budget

    @property
    def toolsets(self) -> set[str]:
        return set(self.keywords | self.jev)

    def how(self, name: str) -> str:
        return "both" if name in self.keywords and name in self.jev else "jev" if name in self.jev else "keywords"

    def log_line(self) -> str:
        """Names and milliseconds only, never the message: "calendar (jev), maps (both); jev 212 ms"."""
        opened = ", ".join(f"{n} ({self.how(n)})" for n in sorted(self.toolsets)) or "none"
        if not self.asked:
            return f"{opened}; jev off"
        return f"{opened}; jev {self.ms:.0f} ms" + (f" ({self.error})" if self.error else "")


def route_turn(text: str, previous: str = "", router: Optional[Callable[[str, str], Route]] = None,
               timeout_s: float = TIMEOUT_S) -> TurnToolsets:
    """toolsets_for with where each toolset came from, and without its log line (the caller logs one per turn).
    Blocking. Jev runs in a worker waited on for ``timeout_s`` at most; a router that is None, raises, errs or is
    slower than that adds nothing, and the keywords stand alone."""
    found = frozenset(keyword_toolsets(text, previous))
    if router is None:
        return TurnToolsets(found)
    box: dict[str, Route] = {}
    done = threading.Event()
    t0 = time.perf_counter()

    def run() -> None:
        try:
            box["route"] = router(text, previous)
        except Exception as e:  # noqa: BLE001 — a router that fails adds nothing
            box["route"] = Route(error=type(e).__name__)
        finally:
            done.set()

    threading.Thread(target=run, name="toolset-router", daemon=True).start()
    if not done.wait(max(0.0, timeout_s)):
        return TurnToolsets(found, frozenset(), (time.perf_counter() - t0) * 1000.0, True, "timeout")
    # A router that answers with anything else adds nothing (review, 2026-10-05); so does one that ended without
    # boxing a Route (a BaseException the worker does not catch), and a Route whose toolsets or ms cannot be read:
    # the keywords still stand, and log_line can format it (Lean RoutedTurn P4, 2026-10-05).
    r = box.get("route")
    try:
        if not isinstance(r, Route):
            raise TypeError("not a route")
        jev = frozenset(str(n) for n in r.toolsets) & frozenset(TOOLSETS)
        ms, error = float(r.ms), str(r.error or "")
    except Exception:  # noqa: BLE001 — what Jev sent cannot hurt the turn
        return TurnToolsets(found, frozenset(), (time.perf_counter() - t0) * 1000.0, True, "not a route")
    return TurnToolsets(found, jev, ms, True, error)


def toolsets_for(text: str, previous: str = "", router: Optional[Callable[[str, str], Route]] = None,
                 timeout_s: float = TIMEOUT_S) -> set[str]:
    """Keywords ∪ Jev: the toolsets a turn starts with. Blocking (call it with asyncio.to_thread). Jev runs in a
    worker waited on for ``timeout_s`` at most; a router that is None, raises, errs or is slower than that adds
    nothing, and the keywords stand alone. Logs the toolsets and Jev's milliseconds, never the message."""
    r = route_turn(text, previous, router, timeout_s)
    if r.asked:
        log.info("toolset router: %s", r.log_line())
    return r.toolsets
