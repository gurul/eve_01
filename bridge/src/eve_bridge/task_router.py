"""The request classifier: which engine should carry a computer-use request.

Every request that reaches `start_task` used to go to the planner (gpt-6-astra: a screenshot,
a 3.4 s turn, a few lines of Python, another turn to say so). The owner's own run logs say what
those requests are (79 runs, 2026-09, ~/.config/eve-bridge/agent-runs):

    about a quarter   "Open Spotify", "Open up a terminal", "open Apple TV on my Mac"   8–32 s each
    about a quarter   "Search Google for …", "pull up the weather in SF on Google"     9–25 s each
    a handful         in-app navigation ("year view and then next year")               10–19 s each
    the rest          looking and telling, typing, playing, ordering, long tasks       10–120 s

The first two rows need no model at all: `open -a Spotify` and one search URL. So the
classifier is tiers, cheapest first, and each tier declines unless it is sure:

    reflex   code only          launch an installed app; open a web search          ~1 s
    lane     code only          lane_router.py: a request one labelled control      ~1–2 s
                                fully accounts for, clicked before the planner
    astra    the planner        everything else: vision, reading, typing, judgement 10 s+
             (+ lane script)    and it may hand the lane exact labels in one turn
             (+ decider)        with EVE_FAST_LANE_DECIDE=model the lane asks a
                                typed-decision model (hosted jev, EVE_DECIDER=jev) on ties

`classify()` is pure: text in, a Plan out, no I/O — so it costs microseconds in the daemon and
is measured offline (tools/route_eval.py) against the owner's real requests. A Plan names the
tiers to try IN ORDER; the last is always astra unless a reflex fully answers the request.

What makes a tier decline is as important as what makes it engage:

- A request that asks to be TOLD something ("tell me the temperature you see") needs eyes. A
  reflex may still do the first part (open the search) and the planner starts from there.
- A CONSEQUENTIAL request (send, order, delete, quit, kill, pay, post, …) is the planner's,
  because only the planner has ask_user. No reflex and no lane touches it.
- A request with text to type ("write this into Warp: …") is the planner's.
- A launch reflex needs the app to be INSTALLED (the same list lane_router uses), and the
  request to be nothing but the launch once the filler is gone ("on my laptop", "for me").
- A search reflex needs explicit search wording and a query. "Pull up something cool" has no
  query a rule should invent; the planner picks one.

Where the typed-decision models fit, by measurement rather than by hope (docs/stackchan/routing.md):
jev as the lane's tie-breaker under the planner, and laya and jev both as candidates for THIS
classifier's fuzzy cases — tools/route_eval.py scores them on the same labelled requests the rules
are scored on.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from typing import Iterable, Optional
from urllib.parse import quote_plus

from .lane_router import is_question, normalise

# The shipped default for EVE_REFLEXES. tools/route_eval.py decides it on the labelled
# requests (precision bar fixed in the tool) and --check-default asserts this constant.
# Decided 2026-09-21 on holdout 3 (110 requests by an author who saw neither the code nor any other
# set; the rules were frozen before it was written; the bar — precision ≥ 0.95 on ≥ 15 fired, zero
# unsafe — was fixed before the first run): 52 fired, 50 right (96.2%), 0 unsafe. It took three
# holdouts: the first two said "disabled" (92.7% with 1 unsafe; 89.1% with 2) and each became a tuning
# set the moment it was read. What the rules still miss is unusual wording ("Notes, please.").
REFLEX_DEFAULT: bool = True
# Which reflexes may fire when they are on: EVE_REFLEXES = 0 | launch | 1. "launch" is the
# narrow policy — ONLY a request that is nothing but a launch of an installed app — and has its own
# default, decided the same way. On holdout 2 every error came from searches and from "launch, then
# more"; the bare launches were 16 of 16. That slice was picked after the fact, so it got its own
# unseen holdout before this constant was allowed to change.
REFLEX_LAUNCH_DEFAULT: bool = True      # holdout 3: 19 bare launches fired, 19 right, 0 unsafe

# Wording that means "this can cost the human something": only the planner, with ask_user, may act.
CONSEQUENTIAL = re.compile(
    r"\b(send|sent|email (?:him|her|them|it)|reply|post|tweet|publish|order|buy|purchase|pay|checkout|book|"
    r"subscribe|delete|remove|erase|trash|empty|uninstall|install|quit|kill|force quit|close|shut ?down|restart|"
    r"log ?out|sign ?out|sign ?in|log ?in|password|transfer|submit|share|upload|format|reset|turn off)\b",
    re.IGNORECASE)
# The request wants an answer in words, so somebody has to look at the screen.
TELL_ME = re.compile(r"\b(tell me|let me know|read (?:it|me|out)|what (?:does|is|are|do)|how (?:many|much)|"
                     r"summari[sz]e|summary|give me (?:the |a )?(?:gist|summary|highlights|rundown|short version)|"
                     r"check (?:if|whether|who|what|my)|and return|report back|say what)\b", re.IGNORECASE)
# Making content is typing, whatever the verb: "add a reminder to call the dentist" (holdout 1).
TYPING = re.compile(
    r"\b(type|write|enter|fill (?:in|out)|paste|dictate|add to notes|take notes|taking notes|note down|jot|compose|draft|"
    r"remind me|(?:add|create|make|start|set) (?:a |an |the |me a |me an )?(?:new )?(?:reminder|event|note|task|to-?do|item|entry|"
    r"contact|appointment|meeting|alarm|timer|playlist|folder|document|doc|file|list|message|email))\b", re.IGNORECASE)
# A request worded as a question out of politeness is still a request ("Can you fire up Slack?").
POLITE = re.compile(r"^(?:hey eve[,\s]*|eve[,\s]*|ok(?:ay)?[,\s]*)?(?:would you mind\s+|do you mind\s+|"
                    r"(?:can|could|would|will) you(?: please| kindly)?\s+|please\s+|i(?:'d| would) like you to\s+|i (?:want|need) you to\s+)+",
                    re.IGNORECASE)
GERUND = {"opening": "open", "launching": "launch", "starting": "start", "bringing up": "bring up",
          "pulling up": "pull up", "firing up": "fire up", "searching": "search", "googling": "google",
          "looking up": "look up"}
# Where a search looks: "… in Slack", "… in my contacts" is inside an app or the owner's own data, not the web.
# Apps whose whole point is writing or sending. "Open Messages and text Jun …", "launch Reminders and
# add pick up the dry cleaning": whatever follows the launch is content or a message, so the request is
# the planner's from its first word (holdout 2's two unsafe reflexes were both this shape).
WRITE_APPS = frozenset({"messages", "mail", "slack", "notes", "reminders", "calendar", "terminal", "warp", "contacts",
                        "textedit", "pages", "discord", "whatsapp", "telegram", "signal", "zoom", "facetime"})
PRONOUN_QUERY = re.compile(r"\b(?:pull|bring|put|open|show) (?:that|it|this|them) up\b|^(?:that|it|this|them)\b", re.IGNORECASE)
LOCAL_SCOPE = re.compile(r"\b(?:in|inside|on|from|through) (?:my|the) (?!web\b|internet\b)[a-z]+(?: [a-z]+)?\s*$", re.IGNORECASE)
FILLER = re.compile(
    r"\b(?:on|in|from) (?:my|the|your|this|the owner'?s) (?:mac(?:book)?|laptop|computer|machine|desktop|screen)\b|"
    r"\bfor me\b|\bright now\b|\bplease\b|\breal quick\b|\bquickly\b|\bnow\b|\bthe app\b|\bapp\b|\bapplication\b|"
    r"\bso (?:that )?(?:it|the app) is .*$|\band bring it to the front.*$|\bso you can .*$", re.IGNORECASE)
LAUNCH_VERB = re.compile(r"^(?:open(?: up)?|launch|start(?: up)?|fire up|bring up|pull up|run|go to|switch to|show(?: me)?)\s+",
                         re.IGNORECASE)
SEARCH_LEAD = re.compile(
    r"^(?:(?:do a |run a )?(?:google|web) search (?:for|on|about)|search (?:google|the web|the internet|online) for|"
    r"search (?:up|for)|look up|google|pull up|bring up|find|search)\s+", re.IGNORECASE)
SEARCH_TAIL = re.compile(r"\s+(?:on|in|using|with|via) (?:google(?: search)?|the web|the internet|a web search|"
                         r"(?:google )?chrome|safari)\s*$", re.IGNORECASE)
NAMES_WEB = re.compile(r"\b(google|web search|the web|the internet|online)\b", re.IGNORECASE)
BROWSERS = {"safari": "Safari", "chrome": "Google Chrome", "google chrome": "Google Chrome", "firefox": "Firefox",
            "arc": "Arc", "brave": "Brave Browser", "edge": "Microsoft Edge"}
IN_BROWSER = re.compile(r"^in (safari|google chrome|chrome|firefox|arc|brave|edge),?\s+", re.IGNORECASE)
AND_THEN = re.compile(r"\s*(?:,\s*)?\b(?:and then|and|then|after that)\b\s*", re.IGNORECASE)
VAGUE_QUERY = re.compile(r"\b(something|anything|some (?:cool|fun|good|random)|a cool|cool (?:shit|stuff|thing)|whatever)\b",
                         re.IGNORECASE)
MAX_QUERY_CHARS = 120
REFLEX_WAIT_SECS = 4.0
SEARCH_URL = "https://www.google.com/search?q="


@dataclass(frozen=True)
class Plan:
    """What to try, in order. `kind` is the first tier's action; `tiers` always ends in astra
    unless a reflex fully answers the request (`complete`)."""

    kind: str                      # launch | search | quit | quit_all | lane | astra
    tiers: tuple[str, ...]         # e.g. ("reflex",) · ("reflex", "astra") · ("lane", "astra") · ("astra",)
    app: str = ""                  # launch: the installed app's name, as installed
    query: str = ""                # search: the query
    browser: str = ""              # search: a browser the request named, "" for the default
    rest: str = ""                 # what is left for the planner after the reflex, in the human's words
    reasons: tuple[str, ...] = field(default_factory=tuple)

    @property
    def complete(self) -> bool:
        """The reflex alone answers the request: no planner, no lane."""
        return self.tiers == ("reflex",)

    def code(self) -> str:
        """The one line the desktop worker runs for a reflex ("" for the other kinds). repr() quotes
        the slot, and the slot is an installed app's own name or a URL-encoded query, never raw speech."""
        if self.kind == "launch" and self.app:
            # 4 s, not the helper's 8: an app that never takes focus (Preview with no document) must not
            # cost the human eight seconds before the planner is even asked (live run, 2026-09-21).
            return f"open_app({self.app!r}, {REFLEX_WAIT_SECS})"
        if self.kind == "search" and self.query:
            url = SEARCH_URL + quote_plus(self.query)
            return f"open_url({url!r}, app={self.browser!r})" if self.browser else f"open_url({url!r})"
        return ""

    def sentence(self) -> str:
        if self.kind == "launch":
            return f"Opened {self.app}."
        if self.kind == "quit":
            return f"Quit {self.app}."
        if self.kind == "search":
            shown = self.query if len(self.query) <= 60 else self.query[:59].rstrip() + "…"
            return f"Here's a search for {shown}."
        return ""


_INSTALLED: Optional[tuple[str, ...]] = None
FINDER = "/System/Library/CoreServices/Finder.app"


def installed_apps(dirs: Iterable[str] = (), listdir=None) -> tuple[str, ...]:
    """The installed apps' names as installed ("Spotify", "TV", "Google Chrome"). Read once per process."""
    import os

    from .lane_router import APP_DIRS

    global _INSTALLED
    default = not dirs and listdir is None
    if default and _INSTALLED is not None:
        return _INSTALLED
    names: list[str] = []
    for d in (dirs or APP_DIRS):
        try:
            entries = (listdir or os.listdir)(os.path.expanduser(d))
        except OSError:
            continue
        names.extend(e[:-4] for e in entries if e.endswith(".app"))
    if default and os.path.isdir(FINDER):
        names.append("Finder")          # in CoreServices, not an app folder; "open finder" is a launch like any other
    found = tuple(sorted(set(names)))
    if default:
        _INSTALLED = found
    return found


def _strip_filler(text: str) -> str:
    out = FILLER.sub(" ", text)
    return " ".join(out.replace(" ,", ",").split()).strip(" ,.!")


def _display_names(apps: Iterable[str]) -> dict[str, str]:
    """{case-folded name: the name as given}. lane_router.installed_app_names() gives folded names
    only, so a folded name is title-cased for display; callers with real names pass them as they are."""
    out: dict[str, str] = {}
    for name in apps:
        clean = " ".join(str(name).split())
        if clean:
            out[clean.casefold()] = clean if clean != clean.casefold() else clean.title()
    return out


def match_app(phrase: str, apps: Iterable[str]) -> str:
    """The installed app `phrase` names, exactly — "spotify" → "Spotify", "apple tv" → "TV" is NOT
    guessed. An alias table holds the few names people say differently from what is installed."""
    names = _display_names(apps)
    said = " ".join(re.findall(r"[a-z0-9.+&-]+", phrase.casefold()))
    if not said:
        return ""
    if said in names:
        return names[said]
    alias = APP_ALIASES.get(said)
    if alias and alias.casefold() in names:
        return names[alias.casefold()]
    # "photobooth" → "Photo Booth": the same letters with the spaces dropped, and only when one app has them.
    squashed = said.replace(" ", "")
    joined = [shown for folded, shown in names.items() if folded.replace(" ", "") == squashed]
    return joined[0] if len(joined) == 1 else ""


# What people say → what is installed. Closed and small on purpose: a wrong alias opens the wrong app.
APP_ALIASES = {"apple tv": "TV", "chrome": "Google Chrome", "settings": "System Settings",
               "system preferences": "System Settings", "preferences": "System Settings", "apple music": "Music",
               "itunes": "Music", "vs code": "Visual Studio Code", "vscode": "Visual Studio Code",
               "imessage": "Messages"}


# Quitting (owner, 2026-09-23): only the explicit word QUIT, a graceful quit (Cmd+Q: an app with unsaved
# work asks to save), never "close", "exit", "kill" or "force quit" — those stay the planner's. "quit all"
# quits every regular app except the ones eve works through (app_reflex.QUIT_ALL_KEEP).
QUIT_VERB = re.compile(r"^(?:go ahead and |just )?quit(?: out of)?\s+", re.IGNORECASE)
QUIT_ALL_TARGET = re.compile(
    r"^(?:all|everything|every ?thing|all of (?:it|them)|every (?:app|application|program)|"
    r"all (?:of )?(?:my |the |the open |my open |open |running |the running )?(?:apps|applications|programs)"
    r"(?: (?:that are|i have|i've got) (?:open|running))?)$", re.IGNORECASE)
FORCE = re.compile(r"\bforce\b", re.IGNORECASE)
QUIT_WORD = re.compile(r"\bquit\b", re.IGNORECASE)


def _quit(text: str, apps: Iterable[str]) -> Optional[str]:
    """The installed app a request asks to quit, "*" for quit all, None when it is not nothing but that."""
    if FORCE.search(text):
        return None
    t = _strip_filler(text)
    if AND_THEN.search(t) and not QUIT_ALL_TARGET.match(QUIT_VERB.sub("", t, count=1).strip()):
        return None                                     # "quit Spotify and open Music": the planner's
    m = QUIT_VERB.match(t)
    if not m:
        return None
    target = re.sub(r"^(?:the|my)\s+", "", t[m.end():].strip(" ,.!"), flags=re.IGNORECASE)
    if QUIT_ALL_TARGET.match(target):
        return "*"
    return match_app(target, apps) or None


def _first_clause(text: str) -> tuple[str, str]:
    """("open spotify", "play don toliver") from "open spotify and play don toliver"."""
    parts = AND_THEN.split(text, maxsplit=1)
    return parts[0].strip(" ,."), (parts[1].strip(" ,.") if len(parts) > 1 else "")


def _launch(text: str, apps: Iterable[str]) -> Optional[tuple[str, str]]:
    """(app, rest) when the first clause is nothing but a launch of an installed app."""
    head, rest = _first_clause(_strip_filler(text))
    m = LAUNCH_VERB.match(head)
    if not m:
        return None
    target = _strip_filler(head[m.end():])
    target = re.sub(r"^(?:the|a|an|my)\s+", "", target, flags=re.IGNORECASE)
    app = match_app(target, apps)
    if not app:
        return None
    return app, _strip_filler(rest)


IN_APP_SEARCH = re.compile(r"^(?:search|look)\s+(?:in(?:side)?\s+|through\s+|on\s+)?(.+?)\s+for\s+", re.IGNORECASE)


def _search(text: str, apps: Iterable[str] = ()) -> Optional[tuple[str, str, str]]:
    """(query, browser, rest) when the request is an explicit web search with a real query."""
    browser = ""
    m = IN_BROWSER.match(text)
    if m:
        browser = BROWSERS.get(m.group(1).casefold(), "")
        text = text[m.end():]
    inside = IN_APP_SEARCH.match(text)
    if inside and match_app(inside.group(1), apps) and inside.group(1).casefold() not in ("google", "the web"):
        return None                                    # "search Spotify for jazz" is a search INSIDE that app
    bare = _strip_filler(text).rstrip(" .!?")
    for prep in re.finditer(r"\b(?:in|inside|on|from|through) ", bare, re.IGNORECASE):
        where = match_app(bare[prep.end():], apps)     # every "in …" suffix, so "from Harbor Supply in Mail" finds Mail
        if where and where not in BROWSERS.values():
            return None                                # "search for the budget thread in Slack": inside that app
    if re.match(r"^(?:search|look (?:in|through)) (?:my|our) ", text, re.IGNORECASE):
        return None                                    # "search my photos for the beach trip": the owner's own data
    if LOCAL_SCOPE.search(_strip_filler(text).rstrip(" .!")):
        return None                                    # "look up Priya in my contacts": the owner's own data
    lead = SEARCH_LEAD.match(text)
    if not lead:
        return None
    verb = lead.group(0).casefold()
    body = text[lead.end():]
    explicit = "search" in verb or verb.startswith(("google", "look up"))
    if not explicit and not NAMES_WEB.search(body):
        return None                                    # "pull up the budget" is not a web search unless it says so
    head, rest = (body, "")
    tell = TELL_ME.search(body)
    if tell:
        head, rest = body[:tell.start()], body[tell.start():]
        head = re.sub(r"\s*\b(?:and|then|,)\s*$", "", head.strip(), flags=re.IGNORECASE)
    head = _strip_filler(head)
    tail = SEARCH_TAIL.search(head)
    if tail:
        named = tail.group(0).casefold()
        for key, value in BROWSERS.items():
            if key in named and not browser:
                browser = value
        head = head[:tail.start()]
    query = _strip_filler(head)
    query = re.sub(r"^(?:for|about|up)\s+", "", query, flags=re.IGNORECASE)
    # "an article on Google about the summit": the engine's name goes, the words after it stay
    query = re.sub(r"\s+(?:on|in|using|from) (?:google|the web|the internet)\b", "", query, flags=re.IGNORECASE)
    query = " ".join(query.split()).strip(" ,.'")
    if query.count('"') % 2:
        query = query.replace('"', "")
    query = query.strip()
    if len(query) < 3 or len(query) > MAX_QUERY_CHARS or VAGUE_QUERY.search(query) or PRONOUN_QUERY.search(query):
        return None                                    # "Google Maps, pull that up" has no query, only a pointer
    return query, browser, _strip_filler(rest)


# ---- a typed-decision model behind the code gates ------------------------------------------------
#
# What a model is for here is the wording the rules do not know ("Notes, please.", "Take me to
# Safari."). It is asked only AFTER the code gates (consequential, typing, a question) have passed the
# request and the rules have found no reflex, and it may only add a BARE LAUNCH of an app that is
# really installed. How it is asked matters more than which model it is (typed_ask.py): Jev asked one
# relative question fired 8 unsafe reflexes in 100 requests; asked its own way — absolute nouls in one
# request, the app chosen from the installed list — it fired none in 110 and lifted the coverage of the
# bare reflexes from 74% to 96%. `model(goal, apps) -> app name or ""`; a failure is an abstention.
ROUTER_MODELS = ("off", "jev")
ROUTER_MODEL_DEFAULT = "off"           # hosted: the request's words leave the Mac, so it is the owner's switch


def find_app_mention(text: str, apps: Iterable[str]) -> str:
    """The ONE installed app the text mentions as whole words (aliases included), "" for none or two."""
    found = app_mentions(text, apps)
    return found[0] if len(found) == 1 else ""


def app_mentions(text: str, apps: Iterable[str]) -> list[str]:
    """Every installed app the text mentions as whole words (aliases included), in the order they are said.
    A name inside a longer one that is also said ("Photo" in "Photo Booth") is not counted twice."""
    names = _display_names(apps)
    folded = " " + " ".join(re.findall(r"[a-z0-9.+&-]+", text.casefold())) + " "
    spoken = {**{k: v for k, v in names.items()},
              **{alias: names[target.casefold()] for alias, target in APP_ALIASES.items() if target.casefold() in names}}
    found: dict[str, int] = {}
    for said in sorted(spoken, key=len, reverse=True):
        at = folded.find(f" {said} ")
        if at >= 0:
            if not any(said in longer and folded.find(f" {longer} ") >= 0 for longer in spoken if len(longer) > len(said)):
                found.setdefault(spoken[said], at)
    return sorted(found, key=found.__getitem__)


EXPLICIT_SEARCH = re.compile(r"^(?:(?:do a |run a )?(?:google|web) search\b|search\b|google\b|look up\b)", re.IGNORECASE)


def _explicit_search(text: str) -> bool:
    return EXPLICIT_SEARCH.match(text) is not None


def _consequential(text: str) -> bool:
    """Does the request ask for something only the planner (with ask_user) may do?

    A web search is harmless whatever its query says, so in an explicit search the risky words
    of the QUERY do not count ("google how to reset a Casio watch") — but a later clause that
    itself starts with a risky verb does ("search for earbuds and buy the cheapest pair")."""
    if not _explicit_search(text):
        return CONSEQUENTIAL.search(text) is not None
    clauses = AND_THEN.split(text)
    return any(CONSEQUENTIAL.match(clause.strip()) for clause in clauses[1:])


def classify(goal: str, *, frontmost_app: str = "", apps: Iterable[str] = (), model=None, quit_model=None) -> Plan:
    """The Plan for one request (the module docstring has the rules). Pure unless `model` is given
    (`model(goal, apps) -> app or ""`, asked only when the gates pass and the rules find no reflex) or
    `quit_model` (`quit_model(goal, apps) -> app, "*" or ""`, asked only for a request that says "quit",
    not "force", is no question, and that the quit rules did not take); never raises."""
    installed = tuple(apps)
    raw = " ".join(str(goal or "").split())
    polite = POLITE.match(raw) is not None
    text = normalise(POLITE.sub("", raw))
    for ing, base in GERUND.items():                   # "would you mind opening Preview" → "open Preview"
        if text.casefold().startswith(ing + " "):
            text = base + text[len(ing):]
    reasons: list[str] = []
    if not text:
        return Plan("astra", ("astra",), reasons=("empty",))
    if QUIT_WORD.search(text) and not FORCE.search(text):
        if not is_question(text, polite=polite):
            target = _quit(text.rstrip("?").strip(), installed)
            how = "the quit rules"
            if target is None and quit_model is not None:
                try:
                    said = str(quit_model(text, list(installed)) or "")
                except Exception:  # noqa: BLE001 — a model that fails abstains
                    said = ""
                target = said if said == "*" or said in installed else None
                how = "the model, for wording the quit rules did not know"
            if target == "*":
                return Plan("quit_all", ("reflex",), reasons=(f"{how}: quit every app but eve's own",))
            if target:
                return Plan("quit", ("reflex",), app=target, reasons=(f"{how}: a bare quit of an installed app",))
    if _consequential(text):
        return Plan("astra", ("astra",), reasons=("consequential: only the planner can ask first",))
    if TYPING.search(text) and not _explicit_search(text):
        return Plan("astra", ("astra",), reasons=("text to type: the planner's",))
    # "?" alone is politeness when the request began "can you …"; a question word in front is a question
    question = is_question(text, polite=polite)
    text = text.rstrip("?").strip()
    launched = _launch(text, apps)
    if launched is not None and not question:
        app, rest = launched
        if app.casefold() == " ".join(frontmost_app.casefold().split()) and not rest:
            reasons.append("already frontmost")
        if rest and app.casefold() in WRITE_APPS:
            return Plan("astra", ("astra",), reasons=(f"{app} is for writing or sending: what follows is the planner's",))
        if rest and TELL_ME.search(rest):
            # "open Music and tell me what's playing": the answer is the job, and only the planner has eyes
            return Plan("astra", ("astra",), reasons=("wants an answer in words: needs eyes",))
        if rest:
            return Plan("launch", ("reflex", "astra"), app=app, rest=rest,
                        reasons=(*reasons, "launch, then the planner does the rest"))
        return Plan("launch", ("reflex",), app=app, reasons=(*reasons, "nothing but a launch of an installed app"))
    searched = _search(text, apps)
    if searched is not None:
        query, browser, rest = searched
        if rest or TELL_ME.search(text):
            return Plan("search", ("reflex", "astra"), query=query, browser=browser, rest=rest or "read the result",
                        reasons=("search, then the planner reads it",))
        return Plan("search", ("reflex",), query=query, browser=browser, reasons=("an explicit web search with a query",))
    if question or TELL_ME.search(text):
        return Plan("astra", ("astra",), reasons=("wants an answer in words: needs eyes",))
    if model is not None:
        try:
            app = str(model(text, list(installed)) or "")
        except Exception:  # noqa: BLE001 — a model that fails is an abstention: the lane, then the planner
            app = ""
        if app and app in installed:
            return Plan("launch", ("reflex",), app=app, reasons=("the rules did not know the wording; the model "
                                                                 "named a bare launch of an installed app",))
    return Plan("lane", ("lane", "astra"), reasons=("the lane may try; it declines unless one control covers every word",))
