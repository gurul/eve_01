"""The owner's everyday apps, direct: Gmail, Google Calendar, Google Drive, Google Contacts, GitHub, Google Maps.

Owner's decision, 2026-10-04: the apps eve uses every day go straight to the vendor, not through a broker
(Composio stays for Slack, Notion, Linear and the long tail, composio_tools.py). A third party in the middle
of the owner's mail, calendar and code is one more company holding their tokens, and one more layer to debug.

* Gmail, Calendar, Drive, Contacts: Google's plain REST APIs (google_rest.py), one Google OAuth sign-in for all.
  Google's Workspace MCP servers answer only for a Cloud project in the Workspace Developer Preview Program
  (live 2026-10-04: "Access to this tool requires that your Google Cloud project ... is enrolled"), which a
  personal gmail does not get, so they are not used.
* GitHub: ``api.githubcopilot.com/mcp/``, with a fine-grained personal access token.
* Google Maps: ``mapstools.googleapis.com/mcp`` (Maps Grounding Lite: places, routes, weather), with an API key.

How it fits:

* The client here is a small Streamable HTTP MCP client on httpx (already a dependency): initialize, then
  tools/list and tools/call as JSON-RPC posts whose reply is JSON or an SSE stream. The ``mcp`` SDK (2.3.0)
  would pull in starlette, uvicorn and OpenTelemetry for a client that needs four requests.
* Each server's tools become Responses-API function tools named ``<app>__<tool>`` (``calendar__list_events``),
  so the brain calls them in one round. Their schemas are slimmed (output schemas and vendor extensions dropped,
  descriptions clipped): Calendar's raw list is ~130 KB, mostly output schemas the model never needs.
* Apps open on demand. A turn starts with only the apps its message names ("mail", "PR", "how long to")
  plus ``open_app``, a small tool that loads more apps (several in one call) for the rest of the turn. All
  six apps' tools at once are ~30k tokens on every turn; most turns use none.
* Each app has a policy for calls that change something (``read``: refused and not even offered; ``auto``
  or ``write``: known private routine changes run; ``ask``: every change needs the owner's yes/no).
  Consequential and unclassified changes always ask. A call reads only when the server marks the tool
  ``readOnlyHint`` and its name carries no writing verb, so a mislabelled tool still asks.
* ``execute`` is blocking and never raises: errors come back as ``{"ok": False, "reason": ...}``.

Tokens never reach a log, a repr or the model. Tool arguments may hold mail text, so logs carry tool names,
counts and exception types only. ``apps login google`` (cli.py) does the one-time browser sign-in.
"""

from __future__ import annotations

import base64
import hashlib
import json
import logging
import os
import re
import secrets
import threading
import time
import urllib.parse
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, Callable, Mapping, Optional, Sequence

import httpx

from . import tool_gate

log = logging.getLogger(__name__)

# httpx logs each request line at INFO with its URL, and a Google API's URL carries the owner's words
# (q=from:sam, a calendar search). Those lines are dropped for the apps' hosts; every other host still logs.
APP_HOSTS = ("gmail.googleapis.com", "people.googleapis.com", "www.googleapis.com/calendar", "www.googleapis.com/drive",
             "www.googleapis.com/upload/drive", "mapstools.googleapis.com", "api.githubcopilot.com",
             "oauth2.googleapis.com")


def _quiet_app_urls(record: logging.LogRecord) -> bool:
    try:
        line = record.getMessage()
    except Exception:  # noqa: BLE001 — a record that cannot render is left to the handler
        return True
    return not any(h in line for h in APP_HOSTS)


logging.getLogger("httpx").addFilter(_quiet_app_urls)

CONFIG_DIR = Path("~/.config/eve-bridge")
PROTOCOL_VERSION = "2025-06-18"
DEFAULT_TIMEOUT_SECS = 60.0
SEP = "__"                       # app and tool in a function name: calendar__list_events

_ON = frozenset({"1", "true", "yes", "on"})


@dataclass(frozen=True)
class Server:
    app: str                     # the prefix of its function names, and the key of its policy
    label: str                   # how the owner calls it
    url: str
    auth: str                    # google | github | maps
    policy: str                  # read | auto | write | ask, for a call that changes something
    headers: tuple[tuple[str, str], ...] = ()


# Routine private changes run; sending, deleting, sharing and other consequential changes ask.
# Explicit read/ask overrides stay stricter. Contacts only look (owner, 2026-10-09).
# EVE_APPS_POLICY="gmail=read,github=write" overrides or extends this.
SERVERS: dict[str, Server] = {s.app: s for s in (
    # Google's: their plain APIs (google_rest.py), so no MCP url.
    Server("gmail", "Gmail", "", "google", "auto"),
    Server("calendar", "Google Calendar", "", "google", "auto"),
    Server("drive", "Google Drive", "", "google", "auto"),
    Server("contacts", "Google Contacts", "", "google", "read"),
    # Its toolsets limited to the owner's everyday ones (the full server is 100+ tools), plus single tools for
    # "did CI pass": X-MCP-Tools composes with X-MCP-Toolsets (github-mcp-server docs/remote-server.md); the
    # token needs Actions: read for them. Notifications are left out: GitHub serves them to a classic token only
    # (live 2026-10-04, "403 Resource not accessible by personal access token"). Read-only mode is not used: it
    # would hide writes that the local policy can run or ask about.
    Server("github", "GitHub", "https://api.githubcopilot.com/mcp/", "github", "auto",
           (("X-MCP-Toolsets", "context,repos,issues,pull_requests,users"),
            ("X-MCP-Tools", "actions_list,actions_get,get_job_logs"))),
    # Every Maps tool only looks; read keeps it that way if one ever does not.
    Server("maps", "Google Maps", "https://mapstools.googleapis.com/mcp", "maps", "read"),
)}
# The words that open an app at the start of a turn, before the model asks. A miss costs a whole model round
# (open_app, then the call); a false hit costs only that app's tokens on that turn. So a compound request
# opens every app it needs: "my next class on the calendar and a maps link" is the calendar and Maps at once
# (owner, 2026-10-04: "it should do it fast because of all the integrations"). Bare "today" and "tomorrow"
# open nothing: they ride along with most requests. "finals" never matches "finally" (\b). A bare "build" is
# not GitHub's: "build me an app" is eve's app maker, and GitHub's schemas are the largest (~44 KB).
_APOS = "['\u2019]"
APP_WORDS: dict[str, re.Pattern[str]] = {app: re.compile(p, re.I) for app, p in {
    "gmail": r"\b(e-?mails?|e-?mail(?:ed|ing)|mails?|inbox|gmail|unread|newsletters?|receipts?|sent me|"
             r"(?:write|reply) (?:back )?to (?:him|her|them)|reply (?:to )?(?:that|this|the) (?:message|thread))\b",
    # someone else's address or number: "Sam's email" alone is a message, and "my email address" is the owner's own
    "contacts": r"\b(contacts?|phone numbers?|(?<!my )(?<!your )e-?mail address(?:es)?|(?:his|her|their) (?:number|"
                r"e-?mail address)|\w+" + _APOS + r"s (?:number|phone|cell|e-?mail address)|number for|"
                # "email sam that…": the address comes from the contacts
                r"(?:(?:^|(?<=please )|(?<=pls )|(?<=can you )|(?<=could you )|(?<=and )|(?<=then )|(?<=also ))"
                r"e-?mail|send (?:an? )?e-?mail to|write an e-?mail to) (?!(?:me|it|the|my|this|that|from|about|"
                r"back|to|is|was|address|addresses|and|or|for|i|you)\b)[a-z]\w+)\b",
    "calendar": r"\b(calendars?|meetings?|events?|appointments?|agenda|(?:re)?schedule[ds]?|busy|"
                r"(?:am i|are we|is \w+) free|free (?:at|on|this|next|tomorrow|today|tonight|later|before|after)|"
                r"free ?/ ?busy|availability|invites?|invitations?|rsvp|book (?:a|an|the) (?:slot|time)|"
                r"class(?:es)?|lectures?|labs?|sections?|seminars?|office hours|exams?|midterms?|finals?|"
                r"quiz(?:zes)?|flights?|what" + _APOS + r"?s next|whats next|up next|my day)\b",
    "drive": r"\b(drive(?! (?:to|there|from|home|back|over))|google docs?|docs?|documents?|sheets?|spreadsheets?|"
             r"slides|folders?|pdfs?)\b",
    "github": r"\b(github|gh|repos?|repository|repositories|pull requests?|prs?|commits?|branch(?:es)?|"
              r"ci|builds|(?:the|my|last|latest|ci) build|build (?:pass|passed|fail|failed|failing|broke|broken|"
              r"status|is|was|green|red)|workflows?|actions? (?:run|runs|workflow|failed|failing|passed))\b",
    "maps": r"\b(maps?(?: link)?|directions?|navigate|navigation|routes?|how (?:long|far)|get(?:ting)? (?:there|to)|"
            r"(?:drive|driving|walk|walking|bike|biking|cycle|cycling) (?:to|there|from)|commute|traffic|eta|"
            r"nearby|near me|near (?:the|my|here)|closest|nearest|where" + _APOS + r"?s|where (?:is|are)|"
            r"(?<!e-mail )(?<!email )address(?:es)?|location|weather|forecast|rain(?:ing)?|"
            r"restaurants?|caf(?:e|é)s?|(?:coffee|food|bars?|lunch|dinner|brunch) (?:near|nearby|around))\b",
}.items()}
# An issue is GitHub's only when the message names no other home for it (review, 2026-10-05): "add a linear issue
# for the login bug", "the open issues in linear" and "any issues with my order on uber eats" opened GitHub, the
# largest schemas, on every round of a turn that never used it. Bare "actions" and "merge" ("what actions do I
# need to take", "merge these two docs") are GitHub's only beside a GitHub word above (PR, branch, CI, repo).
GITHUB_ISSUE = re.compile(r"\b(issues?)\b", re.I)
ISSUE_ELSEWHERE = re.compile(r"\b(linear|notion|slack|jira|asana|trello|uber ?eats|instacart|orders?|"
                             r"deliver(?:y|ies))\b", re.I)


# The owner's repository names that open GitHub as a word (github_repos.keyword_names), set when the apps start.
REPO_NAMES: frozenset[str] = frozenset()


def names_app(app: str, text: str) -> bool:
    """Whether a message names this app (APP_WORDS, an issue with no other home, or one of the owner's repository
    names, for GitHub)."""
    text = text or ""
    rx = APP_WORDS.get(app)
    if rx is not None and rx.search(text):
        return True
    if app == "github" and REPO_NAMES:
        from .github_repos import names_repo

        if names_repo(text, REPO_NAMES):
            return True
    return app == "github" and bool(GITHUB_ISSUE.search(text)) and not ISSUE_ELSEWHERE.search(text)


OPEN_TOOL = "open_app"

# The auth kinds whose token is an API key in this header, not a Bearer token.
KEY_HEADERS = {"maps": "X-Goog-Api-Key"}
POLICIES = ("read", "auto", "write", "ask")

READ_VERBS = frozenset({"get", "list", "fetch", "search", "find", "read", "retrieve", "lookup", "check", "count",
                        "describe", "query", "view", "download", "show", "preview", "suggest"})
WRITE_VERBS = frozenset({"send", "create", "delete", "remove", "update", "edit", "modify", "post", "publish",
                         "merge", "transfer", "submit", "cancel", "archive", "trash", "untrash", "reply", "forward",
                         "invite", "share", "add", "set", "move", "upload", "insert", "write", "patch", "put",
                         "execute", "run", "trigger", "mark", "unmark", "label", "unlabel", "assign", "close",
                         "revoke", "grant", "rename", "copy", "import", "sync", "reset", "clear", "purge", "draft",
                         "compose", "schedule", "accept", "decline", "approve", "reject", "respond", "react",
                         "apply", "fork", "push", "dismiss", "lock", "unlock", "star", "unstar", "pin", "unpin",
                         "pay", "buy", "purchase", "charge", "withdraw", "book", "order", "deploy"})
# Words that are also plain nouns: a write only in verb position (the first word). get_draft reads.
NOUN_TOO = frozenset({"label", "draft", "run", "schedule", "star", "pin", "lock", "order"})

DESC_CLIP = 400                  # a tool's description
PROP_CLIP = 160                  # each description inside its schema
RESULT_CLIP = 60_000             # characters of one result handed back to the model
VALUE_CLIP = 60
LINE_CLIP = 300


def open_tool_spec(closed: Sequence[tuple[str, str]]) -> Optional[dict[str, Any]]:
    """open_app over ``closed`` ((name, label) of each toolset not yet open), or None with none closed. Its enum is
    exactly the closed names, so the model can only name a toolset that is there to open. AppsBridge.open_tool
    passes the direct apps; since 2026-10-05 telegram passes Share and the workspace too, when the
    toolset router decides a turn's tools (toolset_router.py)."""
    names = [name for name, _label in closed]
    if not names:
        return None
    return {"type": "function", "name": OPEN_TOOL, "strict": False,
            "description": "Load the tools of the owner's apps for the rest of this turn, then call them. Name "
                           "every app the request needs in one call. Apps: "
                           + "; ".join(f"{name} = {label}" for name, label in closed) + ".",
            "parameters": {"type": "object", "properties": {"apps": {
                "type": "array", "items": {"type": "string", "enum": names}, "minItems": 1}},
                "required": ["apps"]}}


def open_asked(args: Mapping[str, Any]) -> list[str]:
    """The toolsets an open_app call names (``apps``, or the older single ``app``), lower-cased, each once."""
    args = args or {}
    raw = args.get("apps")
    asked = [raw] if isinstance(raw, str) else list(raw) if isinstance(raw, (list, tuple)) else []
    if args.get("app"):
        asked.append(args["app"])
    return list(dict.fromkeys(str(a).strip().lower() for a in asked if str(a or "").strip()))


# -- configuration --
def _truthy(value: Optional[str]) -> bool:
    return (value or "").strip().lower() in _ON


@dataclass(frozen=True)
class AppsConfig:
    enabled: bool
    config_dir: Path = CONFIG_DIR
    timeout_secs: float = DEFAULT_TIMEOUT_SECS
    policy: Mapping[str, str] = field(default_factory=dict)
    environ: Mapping[str, str] = field(default_factory=dict)

    @property
    def google_client_path(self) -> Path:
        return self.config_dir.expanduser() / "google-oauth-client.json"

    @property
    def google_token_path(self) -> Path:
        return self.config_dir.expanduser() / "google-token.json"


def policy(environ: Optional[Mapping[str, str]] = None) -> dict[str, str]:
    """Each app's policy for writing calls: the defaults, then EVE_APPS_POLICY on top."""
    env = os.environ if environ is None else environ
    out = {app: s.policy for app, s in SERVERS.items()}
    for piece in (env.get("EVE_APPS_POLICY") or "").split(","):
        app, sep, value = piece.strip().lower().partition("=")
        if not sep or not app:
            continue
        if value not in POLICIES:
            log.warning("apps: EVE_APPS_POLICY has %r for %s; one of %s", value, app, POLICIES)
            continue
        out[app] = value
    return out


def configured(environ: Optional[Mapping[str, str]] = None) -> AppsConfig:
    """On with EVE_APPS=1. Each app then comes up only if its sign-in is there (``credentials``)."""
    env = dict(os.environ if environ is None else environ)
    try:
        timeout = float(env.get("EVE_APPS_TIMEOUT_SECS") or DEFAULT_TIMEOUT_SECS)
    except ValueError:
        timeout = DEFAULT_TIMEOUT_SECS
    return AppsConfig(enabled=_truthy(env.get("EVE_APPS")),
                      config_dir=Path(env.get("EVE_APPS_DIR") or CONFIG_DIR),
                      timeout_secs=timeout if timeout > 0 else DEFAULT_TIMEOUT_SECS,
                      policy=policy(env), environ=env)


# -- tokens --
def _read_json(path: Path) -> dict[str, Any]:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return {}
    return data if isinstance(data, dict) else {}


def write_private_json(path: Path, data: Mapping[str, Any]) -> None:
    """Atomically, owner-only (0600): these files hold tokens."""
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + ".tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        json.dump(dict(data), f, indent=2)
        f.write("\n")
    os.chmod(tmp, 0o600)
    os.replace(tmp, path)


GOOGLE_AUTH_URL = "https://accounts.google.com/o/oauth2/v2/auth"
GOOGLE_TOKEN_URL = "https://oauth2.googleapis.com/token"


def google_scopes(pol: Mapping[str, str]) -> list[str]:
    """The Google scopes the policy needs, no wider: Gmail reads, and sends (gmail.send: sending only, no
    deleting or labelling) unless it is read only; the calendar's own scope covers listing calendars and changing
    events; Drive reads everything and writes only files eve made; contacts are read, saved and other."""
    g = "https://www.googleapis.com/auth/"
    scopes = [g + "gmail.readonly"]
    if pol.get("gmail", "ask") != "read":
        scopes += [g + "gmail.send"]
    scopes += [g + "calendar" if pol.get("calendar", "write") != "read" else g + "calendar.readonly"]
    scopes += [g + "drive.readonly"] + ([g + "drive.file"] if pol.get("drive", "ask") != "read" else [])
    scopes += [g + "contacts.readonly", g + "contacts.other.readonly"]
    return scopes


# The kinds whose secret is a fixed value in the env file, read as it is.
ENV_TOKENS = {"github": "EVE_GITHUB_MCP_TOKEN", "maps": "EVE_GOOGLE_MAPS_KEY"}


class TokenSource:
    """One app's bearer token, refreshed when it is about to expire. ``__call__`` returns the token or None
    (not signed in); it never raises past a log line, so one app's broken sign-in never stops the others."""

    def __init__(self, kind: str, config: AppsConfig, http: Optional[httpx.Client] = None) -> None:
        self.kind = kind
        self.config = config
        self._http = http
        self._lock = threading.Lock()

    def __repr__(self) -> str:
        return f"TokenSource({self.kind})"

    @property
    def available(self) -> bool:
        if self.kind in ENV_TOKENS:
            return bool((self.config.environ.get(ENV_TOKENS[self.kind]) or "").strip())
        if self.kind == "google":
            return bool(_read_json(self.config.google_token_path).get("refresh_token"))
        return False

    def __call__(self) -> Optional[str]:
        if self.kind in ENV_TOKENS:
            return (self.config.environ.get(ENV_TOKENS[self.kind]) or "").strip() or None
        with self._lock:
            try:
                return self._google() if self.kind == "google" else None
            except Exception as e:  # noqa: BLE001 — the type only; a token is never logged
                log.warning("apps: %s token refresh failed (%s)", self.kind, type(e).__name__)
                return None

    def _post(self, url: str, data: Mapping[str, str]) -> dict[str, Any]:
        client = self._http or httpx.Client(timeout=30)
        try:
            r = client.post(url, data=dict(data))
            body = r.json()
        finally:
            if self._http is None:
                client.close()
        if not isinstance(body, dict):
            raise ValueError("token endpoint answered with no object")
        return body

    def _google(self) -> Optional[str]:
        path = self.config.google_token_path
        tok = _read_json(path)
        if not tok.get("refresh_token"):
            return None
        if tok.get("access_token") and float(tok.get("expires_at") or 0) > time.time() + 60:
            return str(tok["access_token"])
        client = _google_client(self.config.google_client_path)
        body = self._post(GOOGLE_TOKEN_URL, {"grant_type": "refresh_token", "refresh_token": tok["refresh_token"],
                                             "client_id": client["client_id"],
                                             "client_secret": client.get("client_secret", "")})
        if not body.get("access_token"):
            log.warning("apps: Google refused the refresh (%s); sign in again: apps login google",
                        body.get("error") or "no token")
            return None
        tok.update(access_token=body["access_token"],
                   expires_at=time.time() + float(body.get("expires_in") or 3600))
        write_private_json(path, tok)
        return str(body["access_token"])


def _google_client(path: Path) -> dict[str, Any]:
    """The Desktop OAuth client the owner downloaded from their own Cloud project."""
    data = _read_json(path)
    inner = data.get("installed") or data.get("web") or data
    if not isinstance(inner, dict) or not inner.get("client_id"):
        raise ValueError(f"no OAuth client in {path}")
    return inner


# -- the MCP client --
class McpError(Exception):
    pass


def _sse_messages(text: str) -> list[dict[str, Any]]:
    """The JSON-RPC messages in an SSE body: each event's data lines, joined."""
    out: list[dict[str, Any]] = []
    data: list[str] = []
    for line in text.splitlines() + [""]:
        if line.startswith("data:"):
            data.append(line[5:].lstrip())
        elif not line.strip() and data:
            try:
                msg = json.loads("\n".join(data))
            except ValueError:
                msg = None
            if isinstance(msg, dict):
                out.append(msg)
            data = []
    return out


class McpClient:
    """A Streamable HTTP MCP client, synchronous. One logical session, reopened when the server forgets it."""

    def __init__(self, url: str, token: Callable[[], Optional[str]], headers: Mapping[str, str] | None = None,
                 timeout: float = DEFAULT_TIMEOUT_SECS, http: Optional[httpx.Client] = None,
                 key_header: str = "") -> None:
        self.url = url
        self._token = token
        self._key_header = key_header               # the token goes here, as it is, instead of Authorization
        self._headers = dict(headers or {})
        self._http = http or httpx.Client(timeout=timeout)
        self._session: Optional[str] = None
        self._version = PROTOCOL_VERSION               # what initialize agreed, sent on every later request
        self._ready = False
        self._ids = 0
        self._lock = threading.Lock()

    def _send(self, method: str, params: Optional[Mapping[str, Any]], notify: bool = False) -> dict[str, Any]:
        headers = {"Content-Type": "application/json", "Accept": "application/json, text/event-stream",
                   "MCP-Protocol-Version": self._version, **self._headers}
        token = self._token()
        if token and self._key_header:
            headers[self._key_header] = token
        elif token:
            headers["Authorization"] = f"Bearer {token}"
        if self._session:
            headers["Mcp-Session-Id"] = self._session
        msg: dict[str, Any] = {"jsonrpc": "2.0", "method": method}
        if params is not None:
            msg["params"] = dict(params)
        if not notify:
            self._ids += 1
            msg["id"] = self._ids
        r = self._http.post(self.url, json=msg, headers=headers)
        if r.headers.get("mcp-session-id"):
            self._session = r.headers["mcp-session-id"]
        if r.status_code == 404 and self._session:
            raise _SessionGone()
        if r.status_code in (401, 403):
            raise McpError(f"not signed in or not allowed (HTTP {r.status_code})")
        if r.status_code >= 400:
            raise McpError(f"HTTP {r.status_code}: {r.text[:200]}")
        if notify:
            return {}                                   # 202 Accepted, no body (the transport spec)
        ctype = r.headers.get("content-type", "")
        replies = _sse_messages(r.text) if "text/event-stream" in ctype else [r.json()] if r.content else []
        reply = next((m for m in replies if m.get("id") == msg["id"]), None)
        if reply is None:
            raise McpError(f"no reply to {method}")
        if "error" in reply:
            err = reply["error"] if isinstance(reply["error"], dict) else {"message": str(reply["error"])}
            raise McpError(str(err.get("message") or err)[:300])
        result = reply.get("result")
        return result if isinstance(result, dict) else {}

    def _open(self) -> None:
        self._session = None
        self._version = PROTOCOL_VERSION
        result = self._send("initialize", {"protocolVersion": PROTOCOL_VERSION, "capabilities": {},
                                           "clientInfo": {"name": "eve", "version": "1"}})
        self._version = str(result.get("protocolVersion") or PROTOCOL_VERSION)
        self._send("notifications/initialized", None, notify=True)
        self._ready = True

    def request(self, method: str, params: Optional[Mapping[str, Any]] = None) -> dict[str, Any]:
        with self._lock:
            for attempt in (1, 2):
                if not self._ready:
                    self._open()
                try:
                    return self._send(method, params)
                except _SessionGone:
                    self._ready = False
                    if attempt == 2:
                        raise McpError("the server keeps dropping the session") from None
            raise AssertionError("unreachable")

    def list_tools(self) -> list[dict[str, Any]]:
        tools: list[dict[str, Any]] = []
        cursor: Optional[str] = None
        for _page in range(20):
            result = self.request("tools/list", {"cursor": cursor} if cursor else {})
            tools += [t for t in result.get("tools") or [] if isinstance(t, dict) and t.get("name")]
            cursor = result.get("nextCursor")
            if not cursor:
                break
        return tools

    def call_tool(self, name: str, arguments: Mapping[str, Any]) -> dict[str, Any]:
        return self.request("tools/call", {"name": name, "arguments": dict(arguments or {})})


class _SessionGone(Exception):
    pass


# -- pure: names, schemas, decisions --
def split_name(name: str) -> tuple[str, str]:
    app, sep, tool = (name or "").partition(SEP)
    return (app, tool) if sep and app in SERVERS and tool else ("", "")


def _words(tool: str) -> list[str]:
    return [w for w in tool.replace("-", "_").lower().split("_") if w]


def name_reads(tool: str) -> bool:
    """By its name alone: a reading verb and no writing one (a noun-too word writes only first)."""
    words = _words(tool)
    writes = [w for i, w in enumerate(words) if w in WRITE_VERBS and (w not in NOUN_TOO or i == 0)]
    return any(w in READ_VERBS for w in words) and not writes


def tool_reads(tool: Mapping[str, Any]) -> bool:
    """A tool only looks when the server says so (readOnlyHint) and its name agrees. A server's hint alone never
    makes ``trash_thread`` a read, and with no hint the tool is not a read: the name's words are a closed list,
    so ``find_and_replace`` read by its name alone (Lean AppsPolicy P1, 2026-10-05)."""
    hint = (tool.get("annotations") or {}).get("readOnlyHint") if isinstance(tool.get("annotations"), dict) else None
    words = _words(str(tool.get("name") or ""))
    writes = [w for i, w in enumerate(words) if w in WRITE_VERBS and (w not in NOUN_TOO or i == 0)]
    return hint is True and not writes


def routine_write(app: str, tool: str, args: Optional[Mapping[str, Any]]) -> bool:
    """A closed list of reversible, private operations with understood arguments.

    Tool names alone cannot establish privacy: a file in an existing/shared folder,
    a calendar edit with default guest delivery, or a generic request body asks.
    Missing arguments (legacy callers and workbench scripts) never prove a write routine.
    This is shared with Composio; it does not grant access to a tool or bypass read/ask policy.
    """
    if not isinstance(args, Mapping) or any(not isinstance(k, str) for k in args):
        return False
    normalized = {re.sub(r"[^a-z0-9]", "", k.lower()): v for k, v in args.items()}
    if len(normalized) != len(args):  # ambiguous aliases such as sendUpdates + send_updates
        return False
    direct_calendar = app.lower() == "calendar"
    app = {"googlecalendar": "calendar", "googledrive": "drive", "googledocs": "docs",
           "googlesheets": "sheets"}.get(app.lower(), app.lower())
    name = "_".join(_words(tool))

    def only(fields: set[str]) -> bool:
        return normalized.keys() <= fields

    def plain_values() -> bool:
        # Structured provider request bodies are deliberately outside this allowlist.
        return all(not isinstance(v, Mapping) for v in normalized.values())

    if app == "gmail" and name in {
            "create_draft", "update_draft", "create_email_draft", "update_email_draft", "draft_email",
            "draft_reply"}:
        return only({"draftid", "id", "threadid", "messageid", "replytomessageid", "to", "cc", "bcc",
                     "recipient", "recipients", "subject", "body", "bodytext", "htmlbody", "ishtml",
                     "attachmentids", "userid"}) and plain_values()
    if app == "drive" and name in {"create_file", "upload_file", "create_folder"}:
        # A new item in the owner's root: no parent, existing file id, ACL or sharing options.
        return only({"title", "name", "textcontent", "content", "mimetype", "description", "filetoupload",
                     "filename"}) and plain_values()
    if app == "docs" and name == "create_document":
        return only({"title"}) and plain_values()
    if app == "sheets" and name == "create_spreadsheet":
        return only({"title"}) and plain_values()
    if app == "github" and name == "create_repository":
        # Use the actual adapter key: accepting an ignored alias could create a public repository.
        return args.get("private") is True and only({"name", "description", "private", "autoinit"}) \
            and plain_values()
    if app != "calendar":
        return False
    fields = {"summary", "title", "description", "location", "starttime", "endtime", "startdatetime",
              "enddatetime", "start", "end", "allday", "timezone", "recurrence", "addgooglemeeturl",
              "calendarid", "attendeeemails", "addattendeeemails", "attendees", "notifyguests",
              "sendupdates", "sendnotifications", "eventid"}
    if name == "quick_add_event":
        return only({"text", "calendarid"}) and isinstance(normalized.get("text"), str) \
            and "@" not in normalized["text"] and normalized.get("calendarid", "primary") in (None, "", "primary")
    if name not in {"create_event", "update_event", "patch_event", "events_insert", "events_update", "events_patch"}:
        return False
    if not only(fields) or normalized.get("calendarid", "primary") not in (None, "", "primary"):
        return False
    if any(normalized.get(k) for k in ("attendeeemails", "addattendeeemails", "attendees")):
        return False  # even silent additions share an event with other people
    if normalized.get("notifyguests") not in (None, False) or normalized.get("sendnotifications") not in (None, False):
        return False
    if normalized.get("sendupdates") not in (None, "none"):
        return False
    for key, value in normalized.items():
        if isinstance(value, Mapping) and (key not in {"start", "end"}
                                          or not value.keys() <= {"date", "dateTime", "timeZone"}):
            return False
    if name in {"update_event", "patch_event", "events_update", "events_patch"}:
        # Existing attendees are not in the arguments. The direct adapter otherwise emails them by default.
        if direct_calendar:
            return args.get("notifyGuests") is False
        return args.get("send_updates") == "none"
    return True


def _clip(text: Any, limit: int) -> str:
    text = " ".join(str(text or "").split())
    return text if len(text) <= limit else text[: limit - 1].rstrip() + "…"


def slim_schema(schema: Any) -> Any:
    """An input schema the model needs: vendor extensions, titles and deprecated fields out, descriptions clipped."""
    if isinstance(schema, list):
        return [slim_schema(v) for v in schema]
    if not isinstance(schema, dict):
        return schema
    out: dict[str, Any] = {}
    for k, v in schema.items():
        if k.startswith("x-") or k in ("deprecated", "title", "$schema", "examples"):
            continue
        if k == "description":
            out[k] = _clip(v, PROP_CLIP)
        elif k == "properties" and isinstance(v, dict):
            out[k] = {p: slim_schema(s) for p, s in v.items() if not (isinstance(s, dict) and s.get("deprecated"))}
        else:
            out[k] = slim_schema(v)
    return out


def function_tool(app: str, tool: Mapping[str, Any]) -> dict[str, Any]:
    params = slim_schema(tool.get("inputSchema") or {})
    if not isinstance(params, dict) or params.get("type") != "object":
        params = {"type": "object", "properties": {}}
    params.setdefault("properties", {})
    return {"type": "function", "name": f"{app}{SEP}{tool['name']}"[:64],
            "description": f"[{SERVERS[app].label}] " + _clip(tool.get("description") or tool["name"], DESC_CLIP),
            "parameters": params, "strict": False}


@dataclass(frozen=True)
class Decision:
    action: str                  # run | ask | refuse
    why: str = ""


def _flat(value: Any) -> str:
    if isinstance(value, str):
        return value
    try:
        return json.dumps(value, ensure_ascii=False, default=str)
    except (TypeError, ValueError):
        return str(value)


def _describe(name: str, args: Mapping[str, Any], value_clip: Optional[int]) -> str:
    app, tool = split_name(name)
    what = f"{SERVERS[app].label}: {tool.replace('_', ' ')}" if app else name
    pairs = ", ".join(f"{k}: {_flat(v) if value_clip is None else _clip(_flat(v), value_clip)}"
                      for k, v in (args or {}).items())
    return f"{what} with {pairs}?" if pairs else f"{what}?"


def describe_for_confirmation(name: str, args: Mapping[str, Any]) -> str:
    """One plain line the owner can answer yes or no to: "GitHub: create issue with repo: …, title: …?"."""
    line = _describe(name, args, VALUE_CLIP)
    return _clip(line[:-1], LINE_CLIP - 1) + "?" if len(line) > LINE_CLIP else line


# The writes whose yes is asked on the whole act, never a clipped line, tainted or not: an email is read in full
# before it goes (owner, 2026-10-09). The question can be this long.
WHOLE_ASKS = frozenset({"gmail__send_message"})
WHOLE_ASK_CHARS = 3500


def asks_whole(name: str) -> bool:
    return name in WHOLE_ASKS


def _email_preview(args: Mapping[str, Any]) -> str:
    """An email as the owner reads it before saying yes: who, what, then the body as it will go."""
    lines = ["Send this email?"]
    for key, label in (("to", "To"), ("cc", "Cc"), ("bcc", "Bcc")):
        value = args.get(key)
        if value:
            lines.append(f"{label}: " + (", ".join(map(str, value)) if isinstance(value, (list, tuple)) else str(value)))
    if args.get("replyToMessageId"):
        lines.append("In reply to the message eve read (same thread)")
    lines.append("Subject: " + (str(args.get("subject") or "").strip() or "(Re: the original's)"
                                 if args.get("replyToMessageId") else str(args.get("subject") or "(none)")))
    return "\n".join(lines) + "\n\n" + str(args.get("body") or "")


def describe_whole(name: str, args: Mapping[str, Any], limit: int) -> Optional[str]:
    """The same line with nothing clipped, or None when it is longer than ``limit``: a write a tool result may have
    asked for is shown whole or not asked about at all (tool_gate.py; v3 safety review)."""
    # hidden and bidi characters shown as [U+XXXX], never as nothing (tool_gate.visible; round-4 review)
    if name == "gmail__send_message":
        text = tool_gate.visible(_email_preview(args or {}))   # line breaks kept: an email reads as one
        return text if len(text) <= limit else None
    line = " ".join(tool_gate.visible(_describe(name, args, None)).split())
    return line if len(line) <= limit else None


def result_for_model(result: Mapping[str, Any]) -> dict[str, Any]:
    """An MCP tools/call result as the brain reads it: the structured content when there is one, else the text."""
    out: dict[str, Any] = {"ok": not result.get("isError")}
    structured = result.get("structuredContent")
    if structured is not None:
        out["result"] = structured
    else:
        texts = [str(c.get("text") or "") for c in result.get("content") or []
                 if isinstance(c, dict) and c.get("type") == "text"]
        out["result"] = "\n".join(texts)
        others = [str(c.get("type")) for c in result.get("content") or []
                  if isinstance(c, dict) and c.get("type") != "text"]
        if others:
            out["not_shown"] = others
    raw = _flat(out["result"])
    if len(raw) > RESULT_CLIP:
        out["result"] = raw[:RESULT_CLIP] + "…"
        out["clipped"] = True
    if not out["ok"]:
        out["reason"] = _clip(raw, 300)
    return out


def _google_rest_client(app: str, token: Callable[[], Optional[str]], timeout: float) -> Any:
    from . import google_rest

    return google_rest.GoogleRestClient(app, token, timeout)


# -- the bridge --
class AppsBridge:
    """The owner's connected apps, wrapped so nothing in here raises into the chat loop."""

    def __init__(self, config: AppsConfig,
                 client_factory: Optional[Callable[[Server, Callable[[], Optional[str]]], McpClient]] = None,
                 tokens: Optional[Mapping[str, Callable[[], Optional[str]]]] = None,
                 rest_factory: Optional[Callable[[str, Callable[[], Optional[str]]], Any]] = None) -> None:
        self.config = config
        self.policy = dict(config.policy or policy({}))
        self._factory = client_factory or (lambda s, tok: McpClient(s.url, tok, dict(s.headers), config.timeout_secs,
                                                                      key_header=KEY_HEADERS.get(s.auth, "")))
        self._token_sources = dict(tokens) if tokens is not None else {
            kind: TokenSource(kind, config) for kind in ("google", "github", "maps")}
        self._rest_factory = rest_factory or (lambda app, tok: _google_rest_client(app, tok, config.timeout_secs))
        self._clients: dict[str, Any] = {}                 # McpClient, or google_rest.GoogleRestClient
        self.paths: dict[str, str] = {}                    # app -> "mcp" | "rest", for the log and status
        self._raw: dict[str, dict[str, Any]] = {}          # function name -> the server's own tool
        self._tools: list[dict[str, Any]] = []
        self.repos: list[dict[str, str]] = []              # the owner's GitHub repositories (github_repos.py)

    def available(self) -> list[str]:
        """The apps whose sign-in is there."""
        out = []
        for app, server in SERVERS.items():
            src = self._token_sources.get(server.auth)
            ok = getattr(src, "available", None)
            if src is not None and (ok if ok is not None else True):
                out.append(app)
        return out

    @property
    def started(self) -> bool:
        return bool(self._clients)

    @property
    def apps(self) -> list[str]:
        return list(self._clients)

    def _connect(self, app: str) -> tuple[Any, list[dict[str, Any]], str]:
        """(client, its tools, which path) for one app: Google's three over their plain APIs, the rest over MCP."""
        server = SERVERS[app]
        token = self._token_sources[server.auth]
        if server.auth == "google":
            client = self._rest_factory(app, token)
            return client, client.list_tools(), "rest"
        client = self._factory(server, token)
        return client, client.list_tools(), "mcp"

    def start(self) -> None:
        """List each signed-in app's tools. An app that fails is left out with one log line."""
        for app in self.available():
            server = SERVERS[app]
            try:
                client, listed, path = self._connect(app)
            except Exception as e:  # noqa: BLE001 — the type and a short reason; never a token
                log.warning("apps: %s is not available (%s: %s)", server.label, type(e).__name__, _clip(e, 160))
                continue
            self.paths[app] = path
            offered = 0
            for tool in listed:
                if self.policy.get(app) == "read" and not tool_reads(tool):
                    continue                              # refused anyway: not offered, not paid for each turn
                fn = function_tool(app, tool)
                self._raw[fn["name"]] = dict(tool)
                self._tools.append(fn)
                offered += 1
            self._clients[app] = client
            log.info("apps: %s up (%s), %d of %d tools offered (%s)", server.label, path, offered, len(listed),
                     self.policy.get(app))
        if "github" in self._clients:
            self._load_repos()

    def _load_repos(self) -> None:
        """The owner's repositories, kept a day (github_repos.refresh); their names become GitHub words."""
        global REPO_NAMES
        from . import github_repos

        try:
            self.repos = github_repos.refresh(self.config.config_dir.expanduser() / github_repos.FILE, self.execute)
        except Exception as e:  # noqa: BLE001 — a list that cannot be had costs the line, never the apps
            log.warning("apps: GitHub repositories not loaded (%s)", type(e).__name__)
            self.repos = []
        REPO_NAMES = github_repos.keyword_names(self.repos)

    def tools(self) -> list[dict[str, Any]]:
        return list(self._tools)

    def guess(self, text: str) -> set[str]:
        """The up apps a message names (names_app)."""
        return {app for app in self._clients if names_app(app, text)}

    def open_tool(self, already: set[str]) -> Optional[dict[str, Any]]:
        return open_tool_spec([(app, SERVERS[app].label) for app in self._clients if app not in already])

    def tools_for(self, opened: set[str]) -> list[dict[str, Any]]:
        """One turn's app tools: open_app while some app is still closed, then the opened apps' tools."""
        head = self.open_tool(opened)
        return ([head] if head else []) + [t for t in self._tools if split_name(t["name"])[0] in opened]

    def open(self, args: Mapping[str, Any], opened: set[str]) -> dict[str, Any]:
        """open_app: adds each app named (``apps``, or the older single ``app``) to ``opened`` (the turn's set)
        and names their tools. An app that is not up is reported beside the ones opened, never failing them."""
        wanted = open_asked(args)
        good = [a for a in wanted if a in self._clients]
        unknown = [a for a in wanted if a not in self._clients]
        connected = ", ".join(self._clients)
        if not good:
            return {"ok": False, "reason": f"{', '.join(unknown) or 'no app'} is not connected; connected: {connected}"}
        opened.update(good)
        out: dict[str, Any] = {"ok": True, "opened": good,
                               "tools": [t["name"] for t in self._tools if split_name(t["name"])[0] in good]}
        if unknown:
            out["not_connected"] = unknown
            out["note"] = f"{', '.join(unknown)} is not connected; connected: {connected}"
        return out

    def owns(self, name: str) -> bool:
        """Whether a function name is this bridge's to answer: ``<app>__<tool>`` for an app that is up (a tool it
        lacks too, so decide can say which it has), or open_app while some app is up."""
        if name == OPEN_TOOL:
            return bool(self._clients)
        return split_name(name)[0] in self._clients

    def ensure_open(self, name: str, opened: set[str]) -> None:
        """A call to an up app's tool opens that app for the rest of the turn (the model named it; it gets its
        tools next round, and the call itself runs through decide like any other)."""
        app = split_name(name)[0]
        if app in self._clients:
            opened.add(app)

    def _tool_names(self, app: str) -> list[str]:
        return [t["name"] for t in self._tools if split_name(t["name"])[0] == app]

    @property
    def names(self) -> frozenset[str]:
        return frozenset(self._raw) | ({OPEN_TOOL} if self._clients else frozenset())

    def reads(self, name: str) -> bool:
        tool = self._raw.get(name)
        return tool is not None and tool_reads(tool)

    def prepare(self, name: str, args: Mapping[str, Any]) -> dict[str, Any]:
        """Copy a call, pinning no delivery for a verified guest-free direct calendar edit.

        This blocking read is code-owned and never changes the input or the adapter's sending defaults.
        Pinning the actual flag also prevents guest delivery if guests appear between the read and patch.
        """
        from .google_rest import GoogleRestClient

        prepared = dict(args)
        client = self._clients.get("calendar")
        event_id = prepared.get("eventId")
        if (name != "calendar__update_event" or name not in self._raw
                or self.policy.get("calendar") not in {"auto", "write"}
                or type(client) is not GoogleRestClient or client.app != "calendar"
                or prepared.get("notifyGuests") is not None
                or not isinstance(event_id, str) or not event_id.strip()):
            return prepared
        silent = {**prepared, "notifyGuests": False}
        if not routine_write("calendar", "update_event", silent):
            return prepared
        try:
            # get_event compacts away attendeesOmitted, so inspect the full resource without a field mask.
            event = client._req("GET", f"{client._cal(prepared)}/{urllib.parse.quote(event_id, safe='')}")
        except Exception as e:  # noqa: BLE001 — a failed inspection retains the original ask, never content in logs
            log.info("apps: calendar preparation failed: %s", type(e).__name__)
            return prepared
        if (isinstance(event, dict) and event.get("id") == event_id and event.get("status") != "cancelled"
                and event.get("attendeesOmitted", False) is False
                and isinstance(event.get("attendees", []), list) and not event.get("attendees", [])):
            return silent
        return prepared

    def decide(self, name: str, args: Optional[Mapping[str, Any]] = None) -> Decision:
        """Reads run; explicit read/ask policies remain strict; only understood routine writes run unasked."""
        app, _tool = split_name(name)
        if name not in self._raw and app in self._clients:
            # A name the app does not have, or a change a read-only app never offered: the reason names its
            # real tools, so the next round can call the right one instead of guessing again.
            have = ", ".join(self._tool_names(app)[:60]) or "none"
            if self.policy.get(app) == "read":
                return Decision("refuse", f"{SERVERS[app].label} is read only here; {name} will not run. "
                                          f"Its tools: {have}")
            return Decision("refuse", f"{name} is not one of {SERVERS[app].label}'s tools. Its tools: {have}")
        if name not in self._raw or not app:
            return Decision("refuse", f"{name} is not one of the connected apps' tools")
        if self.reads(name):
            return Decision("run")
        rule = self.policy.get(app, "ask")
        if rule == "read":
            return Decision("refuse", f"{SERVERS[app].label} is read only here; {name} will not run")
        return Decision("run" if rule in {"auto", "write"} and routine_write(app, _tool, args) else "ask")

    def execute(self, name: str, args: Mapping[str, Any]) -> dict[str, Any]:
        """Blocking. Never raises: a failure is a dict the model can read and tell the owner about."""
        app, tool = split_name(name)
        client = self._clients.get(app)
        if client is None or name not in self._raw:
            return {"ok": False, "reason": f"{name} is not available"}
        try:
            return result_for_model(client.call_tool(self._raw[name]["name"], args))
        except Exception as e:  # noqa: BLE001 — the type only: arguments may hold mail text
            log.warning("apps: %s failed: %s", name, type(e).__name__)
            return {"ok": False, "reason": f"{type(e).__name__}: {_clip(e, 200)}"}

    def instructions(self) -> str:
        """For the brain, only while some app is up: which apps, what may be changed in each, and how to reach
        them fast. It rides every turn's prompt, so it is short. Owner's rule (2026-10-04): never write a wrong
        tool *name* here, even as a warning; every name below is one this bridge offers when it is written."""
        if not self._clients:
            return ""
        routine = "routine private changes run; sending, deleting, sharing, publishing and purchases are asked by code"
        rules = {"read": "read only (you may look, never change anything; say so if asked)",
                 "auto": routine, "write": routine, "ask": "changes are asked of the owner first, by code"}
        lines = ["Your owner's own apps are reachable through their tools, named app__tool. A job inside the owner's "
                 "own mail, calendar, Drive, GitHub, or places, routes and weather is done with these tools, which "
                 "answer in seconds, rather than a task on the Mac's screen or a web search. An app whose tools you "
                 "do not have yet is opened first, every app the request needs in one call."]
        for app in self._clients:
            lines.append(f"- {SERVERS[app].label} ({app}__…): {rules[self.policy.get(app, 'ask')]}")
        if "github" in self._clients and self.repos:
            from .github_repos import line

            lines.append(line(self.repos))
        lines.append("Chain apps in the same turn without asking in between.")
        if "calendar" in self._clients:
            lines.append("\"My next class\" (or meeting, flight) is the calendar's event list from now, with no "
                         "calendar named: it reads every calendar the owner sees in one call, and the answer is the "
                         "first upcoming event of that kind. An event with a place already carries mapsUrl and "
                         "directionsUrl, so a Maps link for it needs no maps call.")
        if "gmail" in self._clients and self.policy.get("gmail") != "read":
            lines.append("Gmail sends email, written in the owner's voice; the owner sees each one whole and says "
                         "yes by code. An answer to a message goes into its thread.")
        if "contacts" in self._clients:
            lines.append("Someone's email address or phone number that you do not have comes from Google "
                         "Contacts; never guess an address.")
        if "maps" in self._clients:
            lines.append("The maps tools are for travel time, nearby places and weather. A place, route or weather "
                         "from them is followed by its Google Maps link, as Google's terms ask.")
        lines.append("Keep the links that results carry (event, map, file, PR) in your reply. You do not need to "
                     "ask before a change that is asked by code. Summarise what came back in your own few words; "
                     "never paste a raw record. Anything on the public web is a web search.")
        return "\n".join(lines)


# -- one-time sign-ins (``apps login``) --
def _pkce() -> tuple[str, str]:
    verifier = secrets.token_urlsafe(64)
    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()
    return verifier, challenge


def google_login(config: AppsConfig, open_url: Callable[[str], Any], *, port: int = 0,
                 say: Callable[[str], Any] = print) -> str:
    """The installed-app flow with PKCE on a loopback port: the owner's browser signs in, Google redirects
    to 127.0.0.1, the refresh token is stored 0600. Returns the scopes granted."""
    import http.server

    client = _google_client(config.google_client_path)
    verifier, challenge = _pkce()
    state = secrets.token_urlsafe(16)
    got: dict[str, str] = {}

    class Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self) -> None:  # noqa: N802 — the stdlib's name
            q = dict(urllib.parse.parse_qsl(urllib.parse.urlparse(self.path).query))
            if q.get("state") == state:
                got.update(q)
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.end_headers()
            self.wfile.write(b"eve: signed in. You can close this tab." if q.get("code")
                             else b"eve: no sign-in code here.")

        def log_message(self, *_a: Any) -> None:
            pass

    srv = http.server.HTTPServer(("127.0.0.1", port), Handler)
    redirect = f"http://127.0.0.1:{srv.server_address[1]}/"
    scopes = google_scopes(config.policy or policy({}))
    url = GOOGLE_AUTH_URL + "?" + urllib.parse.urlencode({
        "client_id": client["client_id"], "redirect_uri": redirect, "response_type": "code",
        "scope": " ".join(scopes), "access_type": "offline", "prompt": "consent", "state": state,
        "code_challenge": challenge, "code_challenge_method": "S256"})
    say("Opening Google sign-in in your browser. If it does not open, visit:\n" + url)
    open_url(url)
    srv.timeout = 300
    deadline = time.time() + 300
    while not got.get("code") and not got.get("error") and time.time() < deadline:
        srv.handle_request()
    srv.server_close()
    if not got.get("code"):
        raise RuntimeError(f"no sign-in code ({got.get('error') or 'timed out'})")
    r = httpx.post(GOOGLE_TOKEN_URL, data={"grant_type": "authorization_code", "code": got["code"],
                                           "redirect_uri": redirect, "client_id": client["client_id"],
                                           "client_secret": client.get("client_secret", ""),
                                           "code_verifier": verifier}, timeout=30)
    body = r.json()
    if not body.get("refresh_token"):
        raise RuntimeError(f"Google gave no refresh token ({body.get('error') or r.status_code})")
    write_private_json(config.google_token_path, {
        "refresh_token": body["refresh_token"], "access_token": body.get("access_token", ""),
        "expires_at": time.time() + float(body.get("expires_in") or 3600), "scope": body.get("scope", "")})
    # The owner may untick a permission on Google's page; what was not granted is said, not discovered later.
    missing = sorted(set(scopes) - set(str(body.get("scope", "")).split()))
    if missing:
        say("Not granted (those tools will fail until you sign in again and allow them): " + ", ".join(missing))
    return str(body.get("scope", ""))
