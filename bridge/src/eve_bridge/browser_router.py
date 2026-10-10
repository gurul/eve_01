"""Which body carries a task that is not a reflex: the owner's computer (Codex, or the Chrome lane) or the web reader.

The web reader (web_reader.py, through TinyFish) is a hosted web-reading service: it searches the public web and
reads public pages on its own side, and eve reports what it found as text. It has none of the owner's accounts,
cannot reach the Mac or a local address, and as eve uses it never clicks or types. For a lookup on the public
web it is seconds where driving a browser is tens of seconds, and it never takes over the owner's screen. (The
body before it was an isolated browser, retired 2026-09-23 before it was ever wired in.)

Jev decides, asked the way TypeSafe documents for a routing decision (docs.typesafe.ai: State, Choice
"Structured instructions and criteria", Intent routing, Jev 1.13 jaggedness), and the way typed_ask.py asks it
everywhere else:

* The state is the request and nothing else — no tool manuals in the state ("context rot").
* The two bodies are the options of one Choice, each described as structure: ``what`` it is, ``for``, and
  ``not_for`` — the facts from each tool's own docs, the boundary written into both sides so a literal reader
  cannot confuse them. The wording names no provider, so the service behind the web reader can change without
  a refit.
* Beside the Choice, absolute Nouls, one judgment each (Jev reads literally; "a Choice is relative, each
  Noul is absolute"): does the request need the owner's own accounts or data? anything on the Mac or a local
  address? something shown on the owner's screen? clicking, typing or a form? and is the whole job reading
  public web pages, reported back as text?
* Code combines them and owns the safety rule: the web reader only when the job is reading the public web, needs
  none of the owner's accounts, nothing on the Mac, nothing on screen and no clicking or typing, AND the Choice
  picks it with enough weight. An unsafe route is a task that needs the owner (their accounts, their data,
  their Mac) sent to the web reader: it would fail, and its request text would have gone to a third party. Every
  cut-off is fitted with zero unsafe routes allowed — tools/route_eval.py --browser, on a blind holdout.
"""

from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Callable, Mapping, Optional

Predict = Callable[[Any, dict[str, Any]], dict[str, Any]]

CODEX, WEB = "codex", "web"
AUTO = WEB                        # the name the fitting and eval code use for the non-Codex body

# The two bodies, as the Choice's options. Facts only: the web reader's from its service's docs (search, then read
# public pages on its side, text back) and web_reader.py (no clicks, no forms, no owner accounts);
# the owner's computer from docs/codex-computer-use/README.md and chrome_lane.py.
BODIES: dict[str, Any] = {
    "owner_computer": {
        "what": "an agent that operates the owner's own Mac and the owner's real Chrome browser, already signed "
                "in to the owner's accounts, clicking and typing on the owner's screen",
        "for": ["anything in the owner's own accounts: email, calendar, messages, orders, bookings, banking, "
                "social media, work tools", "anything in a Mac app, a file on the Mac, or a local or staging "
                "website", "clicking through a website, typing into it, filling or submitting a form, or "
                "testing that a website works", "a lookup that needs a number or detail typed into a website, "
                "such as a tracking or confirmation number", "playing music or video for the owner"],
        "not_for": ["only reading public web pages and reporting back what they say"],
    },
    "web_reader": {
        "what": "a web-reading service that searches the public web and reads public web pages in its own "
                "browsers, with no accounts, no cookies and no access to the Mac, and reports what it found "
                "back as text; it does not click, type or fill in anything",
        "for": ["looking up a fact, opening hours, a phone number, a price, a definition or a forecast on "
                "public websites", "comparing products, prices, plans or specifications across public pages",
                "reading a public article, review, status page or documentation page and summarizing it",
                "checking whether a product is listed as in stock on a public store page",
                "collecting information from several public pages into a short answer"],
        "not_for": ["anything that needs the owner signed in, or the owner's own data, numbers or places",
                    "clicking through a site, typing into it, filling in or submitting a form, or testing a site",
                    "anything on the Mac or on a local or private address",
                    "anything the owner wants opened, shown or played on their own screen"],
    },
}
OPTION_TO_BODY = {"owner_computer": CODEX, "web_reader": WEB}


def questions() -> dict[str, Any]:
    return {
        "body": {"type": "choice",
                 "instructions": "Which of the two should carry out the owner's request?",
                 "criteria": BODIES},
        "needs_accounts": {"type": "noul", "instructions": (
            "Does the request need the owner to be signed in to one of their own accounts or services (their email, "
            "calendar, messages, orders, bookings, bank, social media, subscriptions or work tools), or use "
            "something only the owner has or knows (their own numbers, their address, 'my' places or things), or "
            "does it refer to something already open in the owner's browser?")},
        "needs_mac": {"type": "noul", "instructions": (
            "Does the request involve anything outside public websites: an application on the Mac, a file or "
            "folder on the Mac, the desktop or system settings, a local, private or staging web address, or playing "
            "sound or video for the owner to watch or hear?")},
        "show_owner": {"type": "noul", "instructions": (
            "Does the owner want something opened, shown or played on their own screen so they can look at it, read "
            "it or watch it themselves, rather than a result reported back to them?")},
        "needs_interaction": {"type": "noul", "instructions": (
            "Does the request need something put INTO a website: a code, number, date, zip code or name typed into "
            "it, a form filled in or submitted, a sign-up or booking, clicking through a site's steps to reach a "
            "result, or testing whether a website works? Answer no for looking things up, searching the web, "
            "reading, checking or comparing what public pages already show: those put nothing into a site.")},
        "public_read_job": {"type": "noul", "instructions": (
            "Is the whole job finding something out from public websites and telling the owner: looking up a fact, "
            "opening hours, a phone number, a price, a score, a forecast, a definition, a policy or reviews; "
            "checking what a public page says; comparing what several public pages say; or reading and summarizing "
            "a public page, with the answer reported back as text?")},
    }


@dataclass(frozen=True)
class BodyAnswer:
    p_auto: float                 # the Choice's probability for the web_reader option
    accounts: float
    mac: float
    public: float
    show: float = 0.0
    interact: float = 0.0
    ms: float = 0.0
    error: str = ""


@dataclass(frozen=True)
class BodyGates:
    public: float = 0.9
    accounts_max: float = 0.1
    mac_max: float = 0.1
    auto: float = 0.9
    show_max: float = 0.5
    interact_max: float = 0.5


def ask(predict: Predict, goal: str, clock: Callable[[], float]) -> BodyAnswer:
    t0 = clock()
    try:
        result = predict({"request": " ".join(str(goal).split())}, questions())
        answers = result.get("answers") or {}
    except Exception as e:  # noqa: BLE001 — a model that fails abstains: Codex, as before
        return BodyAnswer(0.0, 1.0, 1.0, 0.0, 1.0, 1.0, (clock() - t0) * 1000.0, f"{type(e).__name__}: {e}"[:160])

    def noul(key: str) -> float:
        v = (answers.get(key) or {}).get("noul")
        return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) else 0.0

    probs = (answers.get("body") or {}).get("probabilities") or {}
    p = probs.get("web_reader")
    return BodyAnswer(p_auto=float(p) if isinstance(p, (int, float)) else 0.0, accounts=noul("needs_accounts"),
                      mac=noul("needs_mac"), public=noul("public_read_job"), show=noul("show_owner"),
                      interact=noul("needs_interaction"), ms=(clock() - t0) * 1000.0)


def decide(a: BodyAnswer, g: BodyGates) -> str:
    """WEB only when every condition holds; anything else, including an error, is CODEX."""
    if a.error:
        return CODEX
    if a.accounts > g.accounts_max or a.mac > g.mac_max or a.show > g.show_max or a.interact > g.interact_max:
        return CODEX
    if a.public >= g.public and a.p_auto >= g.auto:
        return WEB
    return CODEX


GRID = (0.5, 0.6, 0.7, 0.8, 0.9, 0.95)
MAX_GRID = (0.05, 0.1, 0.2, 0.3, 0.5)


def fit(answers: list[BodyAnswer], truths: list[str]) -> BodyGates:
    """The cut-offs that route the most WEB-labelled tasks there with ZERO CODEX-labelled tasks sent to
    the web reader. Ties go to the stricter setting."""
    best: Optional[tuple[float, ...]] = None
    chosen = BodyGates(1.01, 0.0, 0.0, 1.01, 0.0, 0.0)
    for public in GRID:
        for auto in GRID:
            for acc in MAX_GRID:
                for mac in MAX_GRID:
                    for show in MAX_GRID:
                        for inter in MAX_GRID:
                            g = BodyGates(public, acc, mac, auto, show, inter)
                            said = [decide(a, g) for a in answers]
                            if any(s == AUTO and t != AUTO for s, t in zip(said, truths, strict=True)):
                                continue
                            right = sum(1 for s, t in zip(said, truths, strict=True) if s == AUTO == t)
                            key = (right, public, auto, -acc, -mac, -show, -inter)
                            if best is None or key > best:
                                best, chosen = key, g
    return chosen


# Fitted 2026-09-26 by tools/route_eval.py --browser on the 122 requests already seen (browser_tuning.json and
# browser_holdout.json, whose one scoring with the first wording held at 12 routed: too few), zero unsafe allowed;
# then scored ONCE on the blind browser_holdout2.json (79 requests, written by an agent that never saw this
# module): 30 routed to the web reader, 30 right (precision 100%), coverage 93.8%, 0 unsafe, Jev p50 198 ms. Refit and
# re-score on a NEW blind set whenever the wording above or the model changes (an alias's answers can change).
GATES = BodyGates(public=0.6, accounts_max=0.1, mac_max=0.1, auto=0.95, show_max=0.5, interact_max=0.2)

# Whether GATES passed the blind holdout's bar (at least 15 routed, precision >= 95%, zero unsafe).
# web_reader.configured's "auto" turns the web body on only when this is True.
SHIPPED = True


def make_router(predict: Predict, clock: Callable[[], float], gates: BodyGates = GATES) -> Callable[[str], str]:
    def route(goal: str) -> str:
        return decide(ask(predict, goal, clock), gates)
    return route


def jev_router(environ: Optional[Mapping[str, str]] = None) -> Optional[Callable[[str], str]]:
    """Jev over EVE_JEV_ROUTE, or None (no key: every task stays with the owner's computer)."""
    import os
    import time

    from . import jev

    try:
        url, key, model = jev.route_config(os.environ if environ is None else environ)
    except Exception:  # noqa: BLE001
        return None
    return make_router(jev.make_predict(url, key, model, timeout_s=2.0), time.perf_counter)
