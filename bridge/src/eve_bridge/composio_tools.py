"""Composio: the text eve's "app hands" for the long tail.

Owner's decision, 2026-10-04: Gmail, Google Calendar, Google Drive and GitHub go direct (apps_mcp.py: Google's
own APIs, GitHub's own MCP server). Composio keeps Slack, Notion, Linear and anything else connected there, and
DIRECT_TOOLKITS are kept out of its session even when connected, so each app has one way in. Gmail and Google
Calendar are the exception that proves it (owner, 2026-10-05: "keep composio for the second gmail"): the owner's
own account is direct, and Composio holds only their OTHER accounts (OTHER_ACCOUNT_TOOLKITS), each call naming one. What follows is the original design, unchanged for those apps.

eve lite (EVE_PROFILE=lite, docs/plans/eve-lite.md M2): Composio Connect is its only way to any app, the owner's own
Gmail and Calendar included, so its verdicts are ``decide_lite``'s: ``decide`` with ``personal_direct=False``, and a
question before COMPOSIO_MANAGE_CONNECTIONS unless the call only checks or links (``manage_needs_yes``).

Owner's decision, 2026-09-21: Gmail, Calendar, GitHub, Notion, Slack and the rest are reachable by API in
seconds through Composio, where driving the Mac's screen for the same job takes minutes. So the Telegram
brain gets Composio's session tools beside its own, and a question like "any mail from Sam today?" is one
API call, not a screen task.

How it fits:

* One Composio *session* per owner. The session's user id is derived from the Telegram owner ids
  (``telegram-<id>[-<id>...]``), so the connected accounts belong to the owner, never to a chat. The
  session id is persisted to ``~/.config/eve-bridge/composio.json`` and resumed on restart; a stale or
  foreign id falls back to a fresh session.
* The session exposes Composio's six meta tools (search, schemas, multi-execute, connections, remote bash,
  remote workbench) already shaped as Responses-API function tools, so they slot into ``telegram.TOOLS``.
* ``execute`` is blocking and never raises: the caller runs it on a thread and hands the dict straight back
  to the model. Errors come back as ``{"ok": False, "reason": "<Type>: message"}``.
* Consequence gating is pure and lives here: ``consequential_slugs`` names the slugs in a call that do
  something (send, create, delete...), by the verb in the slug; ``describe_for_confirmation`` writes the
  yes/no line the owner sees before those run. Read-only verbs (get, list, fetch, search...) run at once.

The SDK is imported lazily inside methods: the daemon must start without ``composio`` installed and with the
feature off. The API key is read from the environment by the SDK only; this module never holds, logs or
repr's it. Tool arguments may hold mail text, so logs carry names, counts and exception types only.
"""

from __future__ import annotations

import ast
import json
import logging
import os
import re
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Mapping, Optional

from . import tool_gate

log = logging.getLogger(__name__)

COMPOSIO_DEFAULT: bool = False
DEFAULT_STATE_PATH = Path("~/.config/eve-bridge/composio.json")
DEFAULT_TIMEOUT_SECS = 60.0

_ON = frozenset({"1", "true", "yes", "on"})

MULTI_EXECUTE = "COMPOSIO_MULTI_EXECUTE_TOOL"
# The alias of the owner's own account on a toolkit with more than one: writes go there unless he names another
# (accounts_note). Set on the connection in Composio (connected_accounts.update(<id>, alias="personal")).
PRIMARY_ALIAS = "personal"
# Toolkits kept out of eve's session. composio_search is Composio's own web search and page fetch: on
# 2026-09-30 the brain used it (COMPOSIO_SEARCH_WEB, COMPOSIO_SEARCH_FETCH_URL_CONTENT, per Composio's execution
# log) to find a course's office hours, which costs a tool search plus a multi-execute per lookup, and ran out
# of rounds one step before adding the events. eve's own web_search (websearch.py, routed by Jev) is one round.
# Disabling them one by one does not hold: with composio_search off, the tool search offered Exa and SerpAPI; with
# those off, Apify, a browser tool, Tavily's MCP variant and Agenty. So the session is made with an allow-list
# instead (TOOLKITS): the owner's kind of apps plus whatever is connected, and nothing on the public web. Checked
# live 2026-09-30: with it, a tool search for "search the web for a course's office hours" offers no web toolkit.
# WEB_TOOLKITS stays as the guard behind it (web_toolkit_slugs), for EVE_COMPOSIO_TOOLKITS=all.
WEB_TOOLKITS: frozenset[str] = frozenset((
    "composio_search", "exa", "serpapi", "serper", "tavily", "linkup", "perplexityai", "firecrawl", "brightdata",
    "scrapingbee", "zenrows", "scrapegraph_ai", "olostep", "agentql", "apify", "hyperbrowser", "browserless",
    "browserbase_tool", "browser_tool", "tavily_mcp", "agenty"))
# The owner's Composio apps; connected toolkits are added at start (but never DIRECT_TOOLKITS). EVE_COMPOSIO_TOOLKITS="gmail,notion" replaces this list; "all" lifts it (start then disables DIRECT_TOOLKITS and a Gmail/Calendar with no other account).
TOOLKITS: tuple[str, ...] = ("slack", "notion", "linear")      # owner, 2026-10-04
# The apps eve reaches directly (apps_mcp.SERVERS). Never in Composio's session, connected there or not, and
# whatever EVE_COMPOSIO_TOOLKITS says: one way in per app.
DIRECT_TOOLKITS: frozenset[str] = frozenset({"googledrive", "github"})
# Apps whose PERSONAL account is direct (apps_mcp) and whose other accounts (the owner's work Gmail and calendar,
# aliased "work" in Composio) stay here. Only an ACTIVE account with an alias other than PRIMARY_ALIAS joins the
# session; an unaliased one cannot be told from the personal one, so it stays out. The session is then made in
# multi-account mode with explicit selection, so Composio never falls back to the personal connection, and decide()
# refuses a call that names no account or the personal one (owner, 2026-10-05).
OTHER_ACCOUNT_TOOLKITS: frozenset[str] = frozenset({"gmail", "googlecalendar"})


def web_toolkit_slugs(name: str, args: Mapping[str, Any]) -> list[str]:
    """The slugs in a multi-execute that belong to a web search or scraping toolkit (the toolkit is the slug's
    prefix, and some toolkit names hold an underscore, so the prefix is matched, not the first word)."""
    if name != MULTI_EXECUTE:
        return []
    out = []
    for tool in _multi_execute_tools(args):
        slug = str(tool.get("tool_slug") or "").strip().upper()
        if any(slug.startswith(t.upper() + "_") for t in WEB_TOOLKITS):
            out.append(slug)
    return out
RUNS_CODE = frozenset({"COMPOSIO_REMOTE_BASH_TOOL", "COMPOSIO_REMOTE_WORKBENCH"})
# Composio's own connection manager: for each toolkit named, a sign-in link when it is not connected, its details when
# it is. Full eve runs it unasked (it changes nothing in the owner's apps); eve lite asks before any call whose shape
# is not that check-or-link one (manage_needs_yes).
MANAGE_CONNECTIONS = "COMPOSIO_MANAGE_CONNECTIONS"
# Its input as Composio serves it (S1, 2026-10-13: tests/fixtures/composio_connect/COMPOSIO_MANAGE_CONNECTIONS.json):
# {toolkits: string[], reinitiate_all?: boolean (default false), session_id?: string}, no other key.
MANAGE_KEYS = frozenset({"toolkits", "reinitiate_all", "session_id"})

# A slug is TOOLKIT_WORDS. A call only looks when one of its words is a reading verb and none is a writing
# verb: GMAIL_FETCH_EMAILS reads, GOOGLECALENDAR_EVENTS_LIST_ALL_CALENDARS reads (the verb is not always the
# first word), GMAIL_SEND_EMAIL writes, GETTY_UPLOAD writes (a verb-like toolkit name proves nothing). What
# happens to a writing call is the toolkit's POLICY below.
READ_VERBS = frozenset({"GET", "LIST", "FETCH", "SEARCH", "FIND", "READ", "RETRIEVE", "LOOKUP", "CHECK", "COUNT",
                        "DESCRIBE", "QUERY", "VIEW", "DOWNLOAD", "SHOW", "PREVIEW"})
WRITE_VERBS = frozenset({"SEND", "CREATE", "DELETE", "REMOVE", "UPDATE", "EDIT", "MODIFY", "POST", "PUBLISH", "PAY",
                         "MERGE", "TRANSFER", "SUBMIT", "CANCEL", "ARCHIVE", "TRASH", "REPLY", "FORWARD", "INVITE",
                         "SHARE", "ADD", "SET", "MOVE", "UPLOAD", "INSERT", "WRITE", "PATCH", "PUT", "EXECUTE", "RUN",
                         "TRIGGER", "MARK", "LABEL", "ASSIGN", "CLOSE", "REVOKE", "GRANT", "RENAME", "COPY", "IMPORT",
                         "SYNC", "RESET", "CLEAR", "PURGE", "BATCH", "DRAFT", "COMPOSE", "SCHEDULE", "BOOK", "ORDER",
                         "ACCEPT", "DECLINE", "APPROVE", "REJECT", "DISABLE", "ENABLE", "STAR", "UNSTAR", "PIN",
                         "BUY", "PURCHASE", "CHARGE", "WITHDRAW", "DEPLOY"})
# Words that are also plain nouns: a write only in verb position (the first word after the toolkit). Live,
# 2026-09-23: GMAIL_GET_LABEL (read the INBOX label, i.e. the unread count) was refused as a write under the
# owner's "gmail: read" policy. Tools that change labels carry a real verb too: ADD_LABEL, MODIFY_…_LABELS.
NOUN_TOO = frozenset({"LABEL", "DRAFT", "ORDER", "RUN", "SCHEDULE"})   # GET_DRAFT, GET_ORDER, GET_A_WORKFLOW_RUN…
READ_ONLY_SLUG = re.compile(r"^[A-Za-z0-9]+_[A-Za-z0-9_]+$")     # the shape; the words decide (is_read_only)

# Per-toolkit policy for a call that WRITES (owner, 2026-09-21: "make gmail read only, allow write for
# calendar"). read: a writing call is refused, never asked; auto/write: understood routine changes run,
# consequential or unknown changes ask; ask: every change asks. Reading calls always run.
# EVE_COMPOSIO_POLICY="gmail=read,googlecalendar=write,slack=ask" overrides or extends this.
DEFAULT_TOOLKIT_POLICY: dict[str, str] = {"gmail": "read", "googlecalendar": "auto", "googledrive": "auto"}
POLICIES = ("read", "auto", "write", "ask")

VALUE_CLIP = 60
LINE_CLIP = 300


@dataclass(frozen=True)
class ComposioConfig:
    """What the bridge needs. No key here: the SDK reads ``COMPOSIO_API_KEY`` from the environment itself."""

    enabled: bool
    user_id: str
    state_path: Path = DEFAULT_STATE_PATH
    timeout_secs: float = DEFAULT_TIMEOUT_SECS
    toolkits: Optional[tuple[str, ...]] = TOOLKITS      # the allow-list; None: no limit ("all")


def _truthy(value: Optional[str]) -> bool:
    return (value or "").strip().lower() in _ON


def configured(environ: Optional[Mapping[str, str]] = None, owner_ids: frozenset[int] = frozenset()) -> ComposioConfig:
    """Read the switch and the surroundings. On only when the switch, the key and an owner are all there."""
    env = os.environ if environ is None else environ
    asked = _truthy(env.get("EVE_COMPOSIO")) if "EVE_COMPOSIO" in env else COMPOSIO_DEFAULT
    has_key = bool((env.get("COMPOSIO_API_KEY") or "").strip())
    has_owner = bool(owner_ids)
    enabled = asked and has_key and has_owner
    if asked and not enabled:
        missing = [name for name, ok in (("COMPOSIO_API_KEY", has_key), ("a Telegram owner id", has_owner)) if not ok]
        log.warning("composio: asked for (EVE_COMPOSIO) but missing %s; staying off", " and ".join(missing))
    user_id = "telegram-" + "-".join(str(i) for i in sorted(owner_ids))
    state_path = Path(env.get("EVE_COMPOSIO_STATE") or DEFAULT_STATE_PATH).expanduser()
    try:
        timeout = float(env.get("EVE_COMPOSIO_TIMEOUT_SECS") or DEFAULT_TIMEOUT_SECS)
    except ValueError:
        timeout = DEFAULT_TIMEOUT_SECS
    if timeout <= 0:
        timeout = DEFAULT_TIMEOUT_SECS
    raw = (env.get("EVE_COMPOSIO_TOOLKITS") or "").strip().lower()
    toolkits: Optional[tuple[str, ...]] = TOOLKITS
    if raw == "all":
        toolkits = None
    elif raw:
        toolkits = tuple(dict.fromkeys(t.strip() for t in raw.split(",") if t.strip()))
    return ComposioConfig(enabled=enabled, user_id=user_id, state_path=state_path, timeout_secs=timeout,
                          toolkits=toolkits)


# -- pure: which calls ask first, and how the question reads --
def slug_words(slug: str) -> tuple[str, list[str]]:
    """("gmail", ["FETCH", "EMAILS"]) from GMAIL_FETCH_EMAILS: the toolkit and the words after it."""
    parts = [w for w in (slug or "").strip().upper().split("_") if w]
    if len(parts) < 2:
        return (parts[0].lower() if parts else ""), []
    return parts[0].lower(), parts[1:]


def is_read_only(slug: str) -> bool:
    if not READ_ONLY_SLUG.match(slug or ""):
        return False
    _toolkit, words = slug_words(slug)
    writes = [w for i, w in enumerate(words) if w in WRITE_VERBS and (w not in NOUN_TOO or i == 0)]
    return any(w in READ_VERBS for w in words) and not writes


def toolkit_policy(environ: Optional[Mapping[str, str]] = None) -> dict[str, str]:
    """The per-toolkit policy for writing calls: the defaults, then EVE_COMPOSIO_POLICY on top."""
    env = os.environ if environ is None else environ
    policy = dict(DEFAULT_TOOLKIT_POLICY)
    for piece in (env.get("EVE_COMPOSIO_POLICY") or "").split(","):
        toolkit, sep, value = piece.strip().lower().partition("=")
        if not sep or not toolkit:
            continue
        if value not in POLICIES:
            log.warning("composio: EVE_COMPOSIO_POLICY has %r for %s; one of %s", value, toolkit, POLICIES)
            continue
        policy[toolkit] = value
    return policy


def _multi_execute_tools(args: Mapping[str, Any]) -> list[dict[str, Any]]:
    tools = args.get("tools") if isinstance(args, Mapping) else None
    return [t for t in tools if isinstance(t, Mapping)] if isinstance(tools, list) else []


# -- the workbench: code that only computes runs without a question (owner, 2026-09-30) --
#
# "I don't want it to ask me permissions regarding this": eve asked before a workbench script that read the
# calendar results it had just fetched and picked out the NLP lectures. The workbench runs Python in Composio's
# sandbox with helpers preloaded (docs.composio.dev/docs/workbench, 2026-09-30): run_composio_tool, invoke_llm,
# upload_local_file, proxy_execute, web_search, smart_file_extract. run_composio_tool can call any connected
# tool, GMAIL_SEND_EMAIL included, so a blanket yes would walk around the per-toolkit policy. Instead the code is
# read here (parsed, never run): a script that only computes runs; each run_composio_tool slug it names is judged
# like a multi-execute slug; anything that reaches out another way, or hides what it calls, still asks.
WORKBENCH = "COMPOSIO_REMOTE_WORKBENCH"
WORKBENCH_TOOL_HELPER = "run_composio_tool"
WORKBENCH_READ_HELPERS = frozenset({"invoke_llm", "web_search", "smart_file_extract"})
WORKBENCH_ASK_HELPERS = frozenset({"upload_local_file", "proxy_execute"})     # publish a file / a raw API call
# Standard modules for working on data; anything else imported (requests, subprocess, os, socket…) asks.
WORKBENCH_IMPORTS = frozenset({
    "json", "re", "math", "statistics", "datetime", "zoneinfo", "time", "calendar", "collections", "itertools",
    "functools", "operator", "string", "textwrap", "difflib", "decimal", "fractions", "random", "csv", "html",
    "unicodedata", "dataclasses", "typing", "enum", "copy", "pprint", "heapq", "bisect", "base64", "hashlib",
    "uuid", "pandas", "numpy", "dateutil"})
# Names that reach outside the data, or that hide what the code calls. Present at all: ask.
WORKBENCH_BLOCKED_NAMES = frozenset({
    "exec", "eval", "compile", "__import__", "getattr", "setattr", "delattr", "globals", "locals", "vars",
    "importlib", "os", "sys", "subprocess", "socket", "shutil", "requests", "httpx", "urllib", "http",
    "aiohttp", "ftplib", "smtplib", "pathlib", "builtins", "ctypes", "multiprocessing", "threading", "asyncio",
    "pickle", "marshal", "composio", "pd_read_url",
    # Lean AppsPolicy finding 5 (2026-10-05): `__builtins__` (any name starting "_", below) reached exec, and a
    # file written with open() then unpickled ran code. open() itself stays: scripts read Composio's saved
    # results with it (CALENDAR_SCRIPT in the tests); the loaders that run what a file holds are blocked.
    "breakpoint", "input", "attrgetter", "methodcaller"})
# Attributes that run code hidden from this reading: pickle loaders (pd.read_pickle, also from a data: URL; HDF
# stores unpickle object columns), pandas' own expression evaluators, and the operator module's name-by-string
# lookups. Any name or attribute holding "pickle" is blocked as well (workbench_slugs).
WORKBENCH_BLOCKED_ATTRS = frozenset({"eval", "exec", "query", "attrgetter", "methodcaller", "read_hdf", "HDFStore"})
# Helpers that evaluate a string annotation as code (review A2, 2026-10-18): the AST reads `x: "run_composio_tool(…)"`
# or `list["…"]` as a plain string, so the call inside was never judged. typing and functools stay allowed; these
# names, by name, attribute or import alias, ask.
WORKBENCH_EVALS_STRINGS = frozenset({"get_type_hints", "ForwardRef", "evaluate_forward_ref", "get_annotations",
                                     "singledispatch", "singledispatchmethod"})
WORKBENCH_BLOCKED_NAMES = WORKBENCH_BLOCKED_NAMES | WORKBENCH_EVALS_STRINGS
WORKBENCH_BLOCKED_ATTRS = WORKBENCH_BLOCKED_ATTRS | WORKBENCH_EVALS_STRINGS


def workbench_slugs(code: Any) -> Optional[list[str]]:
    """The tool slugs a workbench script calls through run_composio_tool, in order, when the script is plain
    enough to judge; None when it must be asked about (unparseable, a blocked name or import, a helper that
    reaches out, a slug that is not a string literal, any name or attribute starting with "_", or a pickle
    loader)."""
    if not isinstance(code, str) or not code.strip():
        return None
    try:
        tree = ast.parse(code)
    except (SyntaxError, ValueError):
        return None
    slugs: list[str] = []
    for node in ast.walk(tree):
        if isinstance(node, (ast.Import, ast.ImportFrom)):
            mods = [a.name for a in node.names] if isinstance(node, ast.Import) else [node.module or ""]
            if isinstance(node, ast.ImportFrom) and node.level:
                return None
            if any(m.split(".")[0] not in WORKBENCH_IMPORTS for m in mods):
                return None
        elif isinstance(node, ast.Name):
            if (node.id.startswith("_") or node.id in WORKBENCH_BLOCKED_NAMES or node.id in WORKBENCH_ASK_HELPERS
                    or "pickle" in node.id.lower()):
                return None
        elif isinstance(node, ast.Attribute):
            if (node.attr.startswith("_") or node.attr in WORKBENCH_ASK_HELPERS or node.attr == WORKBENCH_TOOL_HELPER
                    or node.attr in WORKBENCH_BLOCKED_ATTRS or "pickle" in node.attr.lower()):
                return None
        elif isinstance(node, ast.keyword) and node.arg == "allow_pickle":
            return None
        elif isinstance(node, ast.alias) and (node.name.startswith("_") or "pickle" in node.name.lower()
                                              or node.name in WORKBENCH_BLOCKED_NAMES
                                              or node.name in WORKBENCH_BLOCKED_ATTRS):
            return None
        elif isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id == WORKBENCH_TOOL_HELPER:
            first = node.args[0] if node.args else next(
                (k.value for k in node.keywords if k.arg in ("tool_slug", "slug", "action")), None)
            if not (isinstance(first, ast.Constant) and isinstance(first.value, str) and first.value.strip()):
                return None
            slugs.append(first.value.strip())
    # The helper's name used any way but a direct call (aliased, passed around) hides which tool runs.
    direct = sum(1 for n in ast.walk(tree) if isinstance(n, ast.Call) and isinstance(n.func, ast.Name)
                 and n.func.id == WORKBENCH_TOOL_HELPER)
    named = sum(1 for n in ast.walk(tree) if isinstance(n, ast.Name) and n.id == WORKBENCH_TOOL_HELPER)
    if named != direct:
        return None
    return slugs


def consequential_slugs(name: str, args: Mapping[str, Any]) -> list[str]:
    """The slugs in this call that change something, in call order. Empty means the call only looks."""
    if name == WORKBENCH:
        inner = workbench_slugs(args.get("code_to_execute") if isinstance(args, Mapping) else None)
        if inner is None:
            return [name]
        return [slug for slug in inner if not is_read_only(slug)]
    if name in RUNS_CODE:
        return [name]
    if name == MULTI_EXECUTE:
        out: list[str] = []
        for tool in _multi_execute_tools(args):
            slug = str(tool.get("tool_slug") or "").strip()
            if slug and not is_read_only(slug):
                out.append(slug)
        return out
    return []


@dataclass(frozen=True)
class Decision:
    action: str                      # run | ask | refuse
    slugs: list[str]                 # the writing slugs that decided it (empty for run)
    why: str = ""                    # one line for the owner or the log


def _personal_account_slugs(name: str, args: Mapping[str, Any]) -> list[str]:
    """The Gmail or Google Calendar slugs in a call that would reach the owner's own account through Composio: a
    multi-execute item naming no account or the personal one, or a workbench script calling one (a script cannot
    name its account). Those are direct now (OTHER_ACCOUNT_TOOLKITS, 2026-10-05)."""
    out: list[str] = []
    if name == MULTI_EXECUTE:
        for tool in _multi_execute_tools(args):
            slug = str(tool.get("tool_slug") or "").strip()
            account = str(tool.get("account") or "").strip().lower()
            if slug_words(slug)[0] in OTHER_ACCOUNT_TOOLKITS and account in ("", PRIMARY_ALIAS):
                out.append(slug)
    elif name == WORKBENCH:
        inner = workbench_slugs(args.get("code_to_execute") if isinstance(args, Mapping) else None) or []
        out = [s for s in inner if slug_words(s)[0] in OTHER_ACCOUNT_TOOLKITS]
    return out


def decide(name: str, args: Mapping[str, Any], policy: Optional[Mapping[str, str]] = None, *,
           personal_direct: bool = True) -> Decision:
    """Reads run; known routine writes follow policy and their arguments. A mixed call takes the
    strictest verdict: refuse beats ask beats run. Opaque remote code and unclassified writes ask.

    ``personal_direct``: the owner's own Gmail and Google Calendar have their own tools (apps_mcp), so a call that
    reaches them through Composio is refused (OTHER_ACCOUNT_TOOLKITS). eve lite passes False: it has no direct apps,
    and Composio is the only way to the owner's own mail and calendar. Every other rule is the same either way."""
    from .apps_mcp import routine_write

    if name == MULTI_EXECUTE and (not isinstance(args, Mapping) or not isinstance(args.get("tools"), list)
            or any(not isinstance(t, Mapping) or not isinstance(t.get("tool_slug"), str)
                   or not t["tool_slug"].strip() for t in args["tools"])):
        return Decision("ask", [name])  # the backend must never receive items the local gate could not inspect
    personal = _personal_account_slugs(name, args) if personal_direct else []
    if personal:
        return Decision("refuse", personal, "that is the owner's own account, reached with its own tools; through "
                        "COMPOSIO only their other account runs, named in \"account\"")
    slugs = consequential_slugs(name, args)
    if not slugs:
        return Decision("run", [])
    pol = dict(DEFAULT_TOOLKIT_POLICY if policy is None else policy)
    verdicts: list[tuple[str, str]] = []
    calls = [(str(t["tool_slug"]).strip(), t.get("arguments")) for t in _multi_execute_tools(args)] \
        if name == MULTI_EXECUTE else [(slug, None) for slug in slugs]
    for slug, arguments in calls:
        if slug not in slugs:
            continue
        toolkit, words = slug_words(slug)
        rule = "ask" if slug in RUNS_CODE else pol.get(toolkit, "auto")
        if rule == "read":
            action = "refuse"
        elif rule in {"auto", "write"} and routine_write(toolkit, "_".join(words), arguments):
            action = "run"
        else:
            action = "ask"
        verdicts.append((slug, action))
    refused = [s for s, v in verdicts if v == "refuse"]
    if refused:
        kits = sorted({slug_words(s)[0] for s in refused})
        return Decision("refuse", refused, f"{', '.join(kits)} is read only here; {', '.join(refused)} will not run")
    asked = [s for s, v in verdicts if v == "ask"]
    if asked:
        return Decision("ask", asked)
    return Decision("run", [], "allowed to write: " + ", ".join(slugs))


def manage_needs_yes(args: Any) -> bool:
    """Whether a COMPOSIO_MANAGE_CONNECTIONS call needs the owner's yes in eve lite. False for exactly the shape that
    only checks or links (S1, 2026-10-13: the tool's own description): a mapping with no key but MANAGE_KEYS,
    ``toolkits`` a non-empty list of non-empty strings, ``reinitiate_all`` absent or False, ``session_id`` absent or
    a string. For a toolkit not connected it answers a sign-in link, for one connected its details. Anything else
    is True: ``reinitiate_all`` replaces live connections, and a shape this code does not know (a remove or rename
    that Connect's live schema may have, a value of the wrong type) is asked, never guessed."""
    if not isinstance(args, Mapping) or not set(args) <= MANAGE_KEYS:
        return True
    toolkits = args.get("toolkits")
    if not isinstance(toolkits, list) or not toolkits or any(not isinstance(t, str) or not t.strip() for t in toolkits):
        return True
    if "reinitiate_all" in args and args["reinitiate_all"] is not False:
        return True
    return "session_id" in args and not isinstance(args["session_id"], str)


# Gmail's sending verbs, in a slug's words: a send, a reply, a forward (GMAIL_SEND_EMAIL, GMAIL_REPLY_TO_THREAD,
# GMAIL_FORWARD_MESSAGE, GMAIL_SEND_DRAFT).
MAIL_SEND_WORDS = frozenset({"SEND", "REPLY", "FORWARD"})


def sends_mail(name: str, args: Mapping[str, Any]) -> bool:
    """Whether a call sends email from Gmail: a multi-execute item of Gmail's whose slug sends, replies or forwards.
    eve lite's mail goes only through Composio, so its question shows such a call whole with the room full eve gives
    the owner's own Gmail (apps_mcp.WHOLE_ASK_CHARS, telegram._app_tool), never a clipped one."""
    if name != MULTI_EXECUTE:
        return False
    for tool in _multi_execute_tools(args):
        toolkit, words = slug_words(str(tool.get("tool_slug") or ""))
        if toolkit == "gmail" and MAIL_SEND_WORDS & set(words):
            return True
    return False


# eve lite only: pandas and numpy are allowed inside a workbench script (WORKBENCH_IMPORTS), and their readers that
# take a URL are a way the owner's data could leave the Mac in a query string. Full eve refuses any workbench script
# that reaches the owner's own Gmail or Calendar (a script cannot name an account, OTHER_ACCOUNT_TOOLKITS), which
# covered this; lite reaches those apps on purpose, so it asks before a script that could carry data off instead.
# These names are blocked in lite only, so full eve's workbench verdicts (the owner golden) do not change.
WORKBENCH_LITE_FETCH: frozenset[str] = frozenset({
    "read_csv", "read_table", "read_fwf", "read_json", "read_html", "read_xml", "read_excel", "read_parquet",
    "read_orc", "read_feather", "read_stata", "read_sas", "read_spss", "read_gbq",
    "genfromtxt", "loadtxt", "DataSource"})


def workbench_reaches_out_lite(code: Any) -> bool:
    """For eve lite: whether a workbench script the shared gate would run can still send the owner's data off the
    Mac, or reads the owner's own mail or calendar. True when it calls a Gmail or Google Calendar slug (full eve
    refuses those in a script; lite asks before a script computes on them), a web-search or scraping toolkit slug
    (data can leave in the URL), or a pandas or numpy reader that fetches a URL. Scripts that import requests, os or
    sockets, alias the helper, or hide a slug already ask (``workbench_slugs`` is None, so ``decide`` did not run)."""
    inner = workbench_slugs(code)
    if inner is None:
        return True
    for slug in inner:
        if slug_words(slug)[0] in OTHER_ACCOUNT_TOOLKITS:
            return True
        up = slug.strip().upper()
        if any(up.startswith(t.upper() + "_") for t in WEB_TOOLKITS):
            return True
    try:
        tree = ast.parse(code if isinstance(code, str) else "")
    except (SyntaxError, ValueError):
        return True
    for node in ast.walk(tree):
        name = (node.attr if isinstance(node, ast.Attribute) else node.id if isinstance(node, ast.Name)
                else node.name if isinstance(node, ast.alias) else None)
        if isinstance(name, str) and name in WORKBENCH_LITE_FETCH:
            return True
    return False


def decide_lite(name: str, args: Mapping[str, Any], policy: Optional[Mapping[str, str]] = None) -> Decision:
    """eve lite's verdict on one Composio call (EVE_PROFILE=lite; telegram._app_tool): ``decide`` with the owner's
    own accounts allowed (``personal_direct=False``: Composio Connect is the only way to them), and the owner's yes
    before a COMPOSIO_MANAGE_CONNECTIONS call unless ``manage_needs_yes`` proves it only checks or links. Reads still
    run, sends and deletes still ask, and an explicit read-only policy still refuses. A workbench script that could
    carry the owner's data off the Mac, or that reads their own mail or calendar, asks too
    (``workbench_reaches_out_lite``): the whole-act question is None for a script, so that ask is a refusal."""
    decision = decide(name, args, policy, personal_direct=False)
    if decision.action != "run":
        return decision
    if name == MANAGE_CONNECTIONS and manage_needs_yes(args):
        return Decision("ask", [name])
    if name == WORKBENCH and workbench_reaches_out_lite(
            args.get("code_to_execute") if isinstance(args, Mapping) else None):
        return Decision("ask", [name])
    return decision


def _clip(text: str, limit: int) -> str:
    text = " ".join(text.split())
    return text if len(text) <= limit else text[: limit - 1].rstrip() + "…"


def _flat(value: Any) -> str:
    if isinstance(value, str):
        return value
    if value is None or isinstance(value, (bool, int, float)):
        return json.dumps(value)
    try:
        return json.dumps(value, ensure_ascii=False, default=str)
    except (TypeError, ValueError):
        return str(value)


def _pairs(arguments: Any, value_clip: Optional[int] = VALUE_CLIP) -> str:
    if not isinstance(arguments, Mapping) or not arguments:
        return ""
    return ", ".join(f"{k}: {_flat(v) if value_clip is None else _clip(_flat(v), value_clip)}"
                     for k, v in arguments.items())


def _describe(name: str, args: Mapping[str, Any], value_clip: Optional[int]) -> str:
    parts: list[str] = []
    if name == MULTI_EXECUTE:
        tools = _multi_execute_tools(args)
        asked = [t for t in tools if not is_read_only(str(t.get("tool_slug") or ""))] or tools
        for tool in asked:
            slug = str(tool.get("tool_slug") or "a tool")
            pairs = _pairs(tool.get("arguments"), value_clip)
            account = str(tool.get("account") or "").strip()
            if account and value_clip is not None:
                account = _clip(account, value_clip)
            slug_on = f"{slug} on {account}" if account else slug   # which account it writes to
            parts.append(f"{slug_on} with {pairs}" if pairs else slug_on)
    elif name == WORKBENCH and (inner := consequential_slugs(name, args)) and inner != [name]:
        parts.append(", ".join(dict.fromkeys(inner)) + " from a workbench script")   # the tools it calls, not the code
    else:
        pairs = _pairs(args, value_clip)
        parts.append(f"{name} with {pairs}" if pairs else name)
    return "Run " + "; and ".join(parts) + "?"


def describe_for_confirmation(name: str, args: Mapping[str, Any]) -> str:
    """One plain line the owner can answer yes or no to. Values clipped, the line clipped, no emoji."""
    line = _describe(name, args, VALUE_CLIP)
    if len(line) > LINE_CLIP:
        line = _clip(line[:-1], LINE_CLIP - 1) + "?"
    return line


def describe_whole(name: str, args: Mapping[str, Any], limit: int) -> Optional[str]:
    """The same line with nothing clipped, or None when it is longer than ``limit``: a write a tool result may have
    asked for is shown whole or not asked about at all (tool_gate.py; v3 safety review). A workbench script's
    arguments are code, not shown in a question, so it is never shown whole: None."""
    if name == WORKBENCH:
        return None
    # hidden and bidi characters shown as [U+XXXX], never as nothing (tool_gate.visible; round-4 review)
    line = " ".join(tool_gate.visible(_describe(name, args, None)).split())
    return line if len(line) <= limit else None


# -- the bridge: one session, resumed across restarts --
def _read_state(path: Path) -> dict[str, Any]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return data if isinstance(data, dict) else {}


def _write_state(path: Path, state: Mapping[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(dict(state), f, indent=2)
        f.write("\n")
    os.chmod(tmp, 0o600)
    os.replace(tmp, path)


def _as_dict(result: Any) -> dict[str, Any]:
    """A SDK response (pydantic-ish), a mapping or anything else, as a JSON-serialisable dict."""
    dump = getattr(result, "model_dump", None)
    if callable(dump):
        data = dump()
    elif isinstance(result, Mapping):
        data = dict(result)
    else:
        data = {"data": result}
    return json.loads(json.dumps(data, default=str))


class ComposioBridge:
    """The owner's Composio session, wrapped so nothing in here raises into the chat loop."""

    def __init__(self, config: ComposioConfig, client_factory: Optional[Callable[[], Any]] = None) -> None:
        self.config = config
        self._client_factory = client_factory
        self._client: Any = None
        self._session: Any = None
        self._tools: list[dict[str, Any]] = []
        # toolkit slug -> the owner's ACTIVE accounts on it, each by alias (or id when it has none); filled by
        # _allowed_toolkits. Two or more on one toolkit puts the session in multi-account mode (start).
        self._accounts: dict[str, list[str]] = {}
        self._disabled: list[str] = sorted(DIRECT_TOOLKITS | OTHER_ACCOUNT_TOOLKITS)   # "all" keeps these out

    @property
    def started(self) -> bool:
        return self._session is not None

    @property
    def session_id(self) -> str:
        return str(getattr(self._session, "session_id", "") or "") if self._session is not None else ""

    def _make_client(self) -> Any:
        if self._client_factory is not None:
            return self._client_factory()
        from composio import Composio
        from composio_openai import OpenAIResponsesProvider

        return Composio(provider=OpenAIResponsesProvider(strict=True), timeout=int(self.config.timeout_secs))

    def start(self) -> None:
        """Resume the persisted session for this owner, or create one and persist it. Raises only if the
        SDK cannot create a session at all (no key, no network): the caller decides what that means."""
        self._client = self._make_client()
        sessions = self._client.sessions
        state = _read_state(self.config.state_path)
        stored = str(state.get("session_id") or "")
        session = None
        # Which toolkits a session has is fixed when it is made, so one made with another list is not resumed.
        allowed = self._allowed_toolkits()
        # "all": no allow-list, but the direct apps and a Gmail/Calendar with no other account are disabled
        # (Lean AppsPolicy P3, 2026-10-05: with no limit at all, Drive, GitHub and the personal Gmail joined).
        toolkits: Any = {"enable": allowed} if allowed is not None else {"disable": self._disabled}
        recorded: Any = allowed if allowed is not None else toolkits        # the state file's form for a list
        multi = self.multi_account
        # Multi-account mode with explicit selection: with two accounts on one app, Composio refuses a call that
        # does not name its account rather than picking one silently. Fixed at creation, like the toolkits.
        extra: dict[str, Any] = ({"multi_account": {"enable": True, "require_explicit_selection": True}}
                                 if multi else {})
        if (stored and state.get("user_id") == self.config.user_id and state.get("toolkits") == recorded
                and bool(state.get("multi_account")) == multi):
            try:
                session = sessions.use(stored)
                log.info("composio: resumed the session for %s", self.config.user_id)
            except Exception as e:  # noqa: BLE001 — the type only
                log.warning("composio: stored session unusable (%s); creating a fresh one", type(e).__name__)
        if session is None:
            session = sessions.create(user_id=self.config.user_id, toolkits=toolkits, **extra)
            _write_state(self.config.state_path, {"user_id": self.config.user_id,
                                                  "session_id": str(getattr(session, "session_id", "")),
                                                  "toolkits": recorded, "multi_account": multi})
            log.info("composio: created a session for %s", self.config.user_id)
        self._session = session
        self._tools = self._load_tools()

    def _allowed_toolkits(self) -> Optional[list[str]]:
        """The session's allow-list: the configured toolkits plus every one the owner has connected (an app
        connected outside this list still works), sorted; None when unlimited ("all"; ``_disabled`` then holds
        what is kept out). A failed lookup of the connected accounts leaves the configured list alone. Also records
        each toolkit's accounts in ``_accounts``.

        Gmail and Calendar stay in only with an ACTIVE account whose alias, stripped and lowercased as ``decide``
        reads an account, is neither empty nor PRIMARY_ALIAS; even when EVE_COMPOSIO_TOOLKITS names them
        (Lean AppsPolicy P3, 2026-10-05: "Personal" joined, and so did a configured Gmail with only the personal
        account, in a session that then did not demand an account per call)."""
        allowed = set(self.config.toolkits or ())
        accounts: dict[str, list[str]] = {}
        try:
            listing = self._client.connected_accounts.list(user_ids=[self.config.user_id])
            for item in getattr(listing, "items", None) or []:
                data = item if isinstance(item, Mapping) else _as_dict(item)
                kit = data.get("toolkit")
                slug = kit.get("slug") if isinstance(kit, Mapping) else getattr(kit, "slug", kit)
                if (isinstance(slug, str) and slug and data.get("status") == "ACTIVE"
                        and slug.lower() not in DIRECT_TOOLKITS):
                    alias = str(data.get("alias") or "")
                    if slug.lower() in OTHER_ACCOUNT_TOOLKITS and alias.strip().lower() in ("", PRIMARY_ALIAS):
                        continue                  # the personal (or an unnamed) account: that one is direct
                    # a direct app's accounts are not Composio's: they never make the session multi-account
                    allowed.add(slug.lower())
                    name = alias or str(data.get("id") or "")
                    if name:
                        accounts.setdefault(slug.lower(), []).append(name)
        except Exception as e:  # noqa: BLE001 — the type only
            log.info("composio: connected accounts not listed (%s); the configured toolkits only", type(e).__name__)
        self._accounts = {k: sorted(v) for k, v in accounts.items()}
        unheld = {k for k in OTHER_ACCOUNT_TOOLKITS if k not in self._accounts}    # no other account to hold
        self._disabled = sorted(DIRECT_TOOLKITS | unheld)
        if self.config.toolkits is None:
            return None
        allowed -= WEB_TOOLKITS | DIRECT_TOOLKITS | unheld
        return sorted(allowed)

    @property
    def multi_account(self) -> bool:
        """Some toolkit has two or more of the owner's accounts connected, or holds an other-account app (its
        personal connection is still in Composio, so every call must name its account)."""
        return any(len(v) > 1 or k in OTHER_ACCOUNT_TOOLKITS for k, v in self._accounts.items())

    def accounts_note(self) -> str:
        """For the brain, only while some toolkit has more than one account: which accounts there are and which
        one each kind of call goes to. Empty otherwise, so a one-account prompt is unchanged."""
        multi = {k: v for k, v in sorted(self._accounts.items()) if len(v) > 1 and k not in OTHER_ACCOUNT_TOOLKITS}
        other = {k: v for k, v in sorted(self._accounts.items()) if k in OTHER_ACCOUNT_TOOLKITS}
        if not multi and not other:
            return ""
        lines = []
        if multi:
            lines.append("More than one account is connected for some apps. Every COMPOSIO_MULTI_EXECUTE_TOOL item "
                         "for these apps must set \"account\" to one of the names below:")
            lines.extend(f"- {kit}: {', '.join(names)}" for kit, names in multi.items())
            lines.append(f"To read (a calendar, mail, files), read every account of that app and say which account "
                         f"each result came from. To create, change or delete, use the account named "
                         f"\"{PRIMARY_ALIAS}\" unless the owner names another one.")
        if other:
            labels = {"gmail": "Gmail", "googlecalendar": "Google Calendar"}
            lines.append("Through COMPOSIO, these apps hold only the owner's other accounts, never their own (their "
                         "own is reached with its own tools). Every COMPOSIO_MULTI_EXECUTE_TOOL item for them sets "
                         "\"account\" to one of these names:")
            lines.extend(f"- {labels.get(kit, kit)} ({kit}): {', '.join(names)}" for kit, names in other.items())
            lines.append("When the owner says \"work\" mail or calendar, it is that account; for \"my mail\" or "
                         "\"my calendar\" without saying which, read both and say which account each result came "
                         "from.")
        return "\n".join(lines)

    def _load_tools(self) -> list[dict[str, Any]]:
        raw = self._session.tools()
        out: list[dict[str, Any]] = []
        for tool in raw or []:
            if not isinstance(tool, Mapping):
                tool = _as_dict(tool)
            if tool.get("type") == "function" and tool.get("name"):
                out.append(dict(tool))
        log.info("composio: %d tool%s", len(out), "" if len(out) == 1 else "s")
        return out

    def tools(self) -> list[dict[str, Any]]:
        return list(self._tools)

    @property
    def names(self) -> frozenset[str]:
        return frozenset(str(t["name"]) for t in self._tools)

    # -- reads for Updates (updates_composio.py): one app tool on one connected account, never a write --
    @property
    def can_read(self) -> bool:
        """The SDK client is up (``start`` got as far as making it), so ``connected_accounts`` and ``read_tool`` can
        run. Composio not installed, or the feature off: False."""
        return self._client is not None

    def connected_accounts(self) -> list[dict[str, str]]:
        """Blocking. The owner's connected accounts as Composio lists them now: ``{"toolkit", "id", "alias",
        "status"}`` each, the toolkit lowercased, DIRECT_TOOLKITS left out. Raises when the listing fails (the caller
        counts that as a failed read). Ids and aliases are never logged."""
        if self._client is None:
            raise RuntimeError("composio is not started")
        listing = self._client.connected_accounts.list(user_ids=[self.config.user_id])
        out: list[dict[str, str]] = []
        for item in getattr(listing, "items", None) or []:
            data = item if isinstance(item, Mapping) else _as_dict(item)
            kit = data.get("toolkit")
            slug = kit.get("slug") if isinstance(kit, Mapping) else getattr(kit, "slug", kit)
            if not isinstance(slug, str) or not slug or slug.lower() in DIRECT_TOOLKITS or not data.get("id"):
                continue
            out.append({"toolkit": slug.lower(), "id": str(data["id"]), "alias": str(data.get("alias") or ""),
                        "status": str(data.get("status") or "")})
        return out

    def read_tool(self, slug: str, arguments: Mapping[str, Any], account_id: str) -> dict[str, Any]:
        """Blocking. Run ONE app tool that only reads on the connected account ``account_id`` (an id from
        ``connected_accounts``), outside the chat's session, so the whole answer comes back (the session's
        multi-execute answers with a preview) and the account is the one named, never a default. Never raises:
        ``{"ok": False, "reason"}`` on a failure. A slug that writes (``is_read_only``, ``consequential_slugs``) is
        refused before anything is sent, as is a direct app's slug and an other-account app's call with no account."""
        slug = str(slug or "").strip()
        toolkit = slug_words(slug)[0]
        if not is_read_only(slug) or consequential_slugs(MULTI_EXECUTE, {"tools": [{"tool_slug": slug}]}):
            return {"ok": False, "reason": f"{slug} is not a read"}
        if toolkit in DIRECT_TOOLKITS or not str(account_id or "").strip():
            return {"ok": False, "reason": f"{slug} needs one of the owner's Composio accounts"}
        if self._client is None:
            return {"ok": False, "reason": "composio is not started"}
        try:
            result = self._client.tools.execute(slug, arguments=dict(arguments or {}), user_id=self.config.user_id,
                                                connected_account_id=str(account_id),
                                                dangerously_skip_version_check=True)
            out = _as_dict(result)
        except Exception as e:  # noqa: BLE001 — the type only: answers may hold mail text
            log.warning("composio: %s read failed: %s", slug, type(e).__name__)
            return {"ok": False, "reason": f"{type(e).__name__}: {_clip(str(e), 200)}"}
        out["ok"] = out.get("successful") is not False and not out.get("error")
        return out

    def execute(self, name: str, args: Mapping[str, Any]) -> dict[str, Any]:
        """Blocking. Never raises: a failure is a dict the model can read and tell the owner about."""
        if self._session is None:
            return {"ok": False, "reason": "composio is not started"}
        try:
            result = self._session.execute(name, arguments=dict(args or {}))
            out = _as_dict(result)
        except Exception as e:  # noqa: BLE001 — the type only: arguments may hold mail text
            log.warning("composio: %s failed: %s", name, type(e).__name__)
            return {"ok": False, "reason": f"{type(e).__name__}: {_clip(str(e), 200)}"}
        out.setdefault("ok", not out.get("error"))
        return out
