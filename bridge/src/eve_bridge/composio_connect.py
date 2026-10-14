"""eve lite's apps: Composio Connect's meta tools over MCP, signed in as the person (docs/plans/eve-lite.md, WP-6).

Owner, 2026-10-13: "use composio and openrouter as the only setup things". eve lite signs in to Composio Connect in
the browser (onboarding.ComposioConnectLogin writes ``composio-connect.json``), and the brain then reaches the person's
apps through Connect's MCP server, ``https://connect.composio.dev/mcp``. Spike S1 (2026-10-13,
docs/plans/eve-lite-s1.md) measured what this module relies on; its captures are tests/fixtures/composio_connect.

* ``ConnectTokenSource`` hands out the access token in the token file. It refreshes the token about a minute before
  it lapses, and once more when Composio answers 401: a form POST to the token endpoint with ``grant_type``,
  ``refresh_token``, ``client_id`` and ``resource``, and no secret (the form S1 verified). The answer is written back
  (0600, atomically) with its refresh token, because the server may rotate it. A refresh token the token endpoint
  refuses (``invalid_grant``, an unknown client) is never sent again: the person signs in again from Settings → Apps.
  A 401 from /mcp to the fresh token too says less: S1's 401 also covers Composio not finding the account, which may
  pass. That sign-in is held for REFUSED_RETRY_SECS (``hold``), then tried again.
* ``ConnectBridge`` is the duck type the brain already reads for Composio (composio_tools.ComposioBridge:
  ``started``, ``tools()``, ``names``, ``execute``, ``accounts_note``), over ``apps_mcp.McpClient``. It offers the
  seven meta tools (META_TOOLS) under their own ``COMPOSIO_*`` names, so the code gate (composio_tools.decide_lite)
  judges every call as it judges full eve's. A tool the server lists beyond those seven is left out: the gate runs a
  name it does not know without asking.
* What the model reads: each input schema slimmed (apps_mcp.slim_schema), and Composio's own description whole, less
  its catalog line (CATALOG_LINE). The descriptions carry the meta tools' protocol (sessions, schemas, the sign-in
  link to send); apps_mcp's 400-character clip would keep COMPOSIO_SEARCH_TOOLS's catalog line and cut all of that.
  Full eve's SDK passes them whole too.
* ``execute`` is blocking and never raises: a failure is ``{"ok": False, "reason": ...}``. Composio's answer is read
  into its own object (``data``, ``error``, ``successful``) with ``ok`` added, the shape full eve's bridge returns. A
  401 refreshes the sign-in once and tries once more, then answers plainly. After a 401 to the fresh token as well,
  no call sends the sign-in or mints a token pair until its hold is over, and the model is told to try again later.
  The model's line, and Settings, ask for a new sign-in only when this one cannot be renewed: the token endpoint
  refused it, or it has no refresh token and its access token is refused or has lapsed. Nothing else is retried: a
  call that timed out may have run.
* ``start()`` returns at once. A thread lists the tools, trying again with backoff (START_BACKOFF) until it works or
  ``close()``. Until then ``started`` is False and the brain offers no app tools.
* Settings → Apps (lite_web.py) has ``connect_link(toolkit)``, the Connect Link that connects one app (read from that
  app's own part of Composio's answer, never another app's; ``https`` on composio.dev or a subdomain, else None), and
  ``connections()``, which apps are connected. That is read from COMPOSIO_SEARCH_TOOLS, which only looks:
  COMPOSIO_MANAGE_CONNECTIONS starts a sign-in for each app it is asked about that is not connected, so it runs only
  when the person asks for a link.

Tokens never reach a log, a repr, a result or the model, and logs carry tool names, counts and exception types only:
arguments and results may hold mail text (AGENTS rule 10). Unlike onboarding's, these tokens are not handed to
``telegram.hide_token``: an access token is replaced many times a day, and hide_token's set only grows.
"""

from __future__ import annotations

import hashlib
import itertools
import json
import logging
import math
import re
import threading
import time
import urllib.parse
from pathlib import Path
from typing import Any, Callable, Iterable, Iterator, Mapping, Optional, Sequence, TypeVar

import httpx

from . import apps_mcp, composio_tools
from .apps_mcp import McpClient, McpError, _read_json, write_private_json
from .onboarding import (
    _TOKEN_SHAPE,
    CONNECT_ISSUER,
    CONNECT_RESOURCE,
    CONNECT_TOKEN_FILE,
    CONNECT_TOKEN_URL,
    TOKEN_FILE_VERSION,
    _answer,
    _expires_at,
    _oauth_word,
)

log = logging.getLogger(__name__)

_T = TypeVar("_T")

MCP_URL = CONNECT_RESOURCE           # Connect's MCP endpoint is also the resource its tokens are minted for (S1)
SEARCH_TOOLS = "COMPOSIO_SEARCH_TOOLS"
MANAGE_CONNECTIONS = "COMPOSIO_MANAGE_CONNECTIONS"
# The seven meta tools Composio Connect serves (S1: the same seven its docs list), in the order they are offered. The
# order is fixed, not the server's, so the prompt's cached prefix is the same from one start to the next.
META_TOOLS: tuple[str, ...] = (SEARCH_TOOLS, "COMPOSIO_GET_TOOL_SCHEMAS", "COMPOSIO_MULTI_EXECUTE_TOOL",
                               MANAGE_CONNECTIONS, "COMPOSIO_WAIT_FOR_CONNECTIONS", "COMPOSIO_REMOTE_WORKBENCH",
                               "COMPOSIO_REMOTE_BASH_TOOL")
# COMPOSIO_SEARCH_TOOLS's description opens with a line selling Composio's catalog, its web search and a browser among
# it. eve never runs those toolkits (composio_tools.WEB_TOOLKITS: the public web is eve's own web_search), so the
# line is left out rather than naming tools no turn can run (the owner's rule for prompts). The rest is Composio's.
CATALOG_LINE = "Tool Server Info:"
DESC_MAX = 8000                      # a longer description is clipped (the longest S1 saw: 4,363 characters)

CALL_TIMEOUT_SECS = 200.0            # the brain's calls: a workbench or bash cell may run 180 s (their descriptions)
OWN_TIMEOUT_SECS = 60.0              # Settings → Apps' calls: a Connect Link; which apps are connected
CONNECT_TIMEOUT_SECS = 15.0
TOKEN_TIMEOUT_SECS = 20.0
REFRESH_EARLY_SECS = 60.0            # a token with less left than this is refreshed before it is sent
REFRESH_RETRY_SECS = 30.0            # no refresh this soon after one that failed for a reason that may pass
REFUSED_RETRY_SECS = 300.0           # how long a sign-in /mcp refused even fresh from a refresh is held (hold)
START_BACKOFF: tuple[float, ...] = (5.0, 15.0, 30.0, 60.0, 120.0, 300.0)   # between start attempts; the last repeats
CONNECTIONS_TTL_SECS = 60.0          # how long connections() trusts what it last read
LOOK_RETRY_SECS = 15.0               # no new read this soon after one that failed
LOOK_MAX = 10                        # apps per COMPOSIO_SEARCH_TOOLS call when connections() reads them

# Settings → Apps (docs/plans/eve-lite.md, flow A step 6): the apps it offers a Connect button for, by Composio
# toolkit slug, in its order, with the name it shows. Any other toolkit with managed sign-in connects too.
APPS: dict[str, str] = {"gmail": "Gmail", "googlecalendar": "Google Calendar", "googledrive": "Google Drive",
                        "slack": "Slack", "notion": "Notion", "linear": "Linear", "github": "GitHub"}
# What connections() asks COMPOSIO_SEARCH_TOOLS for each app: one plain read in it, by name, so the answer's
# toolkit_connection_statuses reports that app. The search only looks: it finds tools and reports each app's
# connection, and changes nothing in the app.
_LOOKS: dict[str, str] = {
    "gmail": "read my latest emails in Gmail", "googlecalendar": "list my upcoming events in Google Calendar",
    "googledrive": "list my recent files in Google Drive", "slack": "list my channels in Slack",
    "notion": "search my pages in Notion", "linear": "list my assigned issues in Linear",
    "github": "list my repositories on GitHub"}

# What the model reads when a call cannot be made. Plain lines it can pass on.
NOT_UP = "Composio is not reachable from this Mac yet, so the apps cannot be used. Try again in a minute."
SIGNED_OUT = ("The Composio sign-in on this Mac has lapsed, so the apps cannot be used. The owner signs in to Composio "
              "again in Settings → Apps.")
NOT_RENEWED = ("Composio did not take the sign-in, and it could not be renewed just now, so the apps cannot be used. "
               "Try again in a few minutes.")
MAY_HAVE_RUN = " The call may have run: check before running it again."
UNREADABLE = ("Composio answered in a form eve could not read. The call may have run: check before running it "
              "again.")

# The token endpoint's errors that mean the refresh token or the client is gone for good (RFC 6749 5.2).
_REFUSED = frozenset({"invalid_grant", "invalid_client", "unauthorized_client"})
_SLUG = re.compile(r"[a-z0-9][a-z0-9_]{0,63}")          # a toolkit slug: gmail, googlecalendar, google_maps
_HOST = re.compile(r"[a-z0-9-]+(?:\.[a-z0-9-]+)*")
# Where a COMPOSIO_MANAGE_CONNECTIONS answer may put a toolkit's sign-in link. Its own description names
# redirect_url; Composio documents no shape beyond the envelope, so the others are read too, and link_ok decides.
_LINK_KEYS = ("redirect_url", "redirectUrl", "connect_link", "connectLink", "link")
_ACTIVE = frozenset({"active", "connected"})
_MAY_HAVE_RUN = (httpx.ReadTimeout, httpx.ReadError, httpx.RemoteProtocolError)   # failed after the call was sent


# -- pure: names, links, answers --
def _clip(text: Any, limit: int) -> str:
    text = " ".join(str(text or "").split())
    return text if len(text) <= limit else text[: limit - 1].rstrip() + "…"


def _text(value: Any) -> str:
    return value if isinstance(value, str) else ""


def _expiry(value: Any) -> float:
    """``expires_at`` as epoch seconds; 0 (lapsed) for anything that is not a finite number."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return 0.0
    try:
        number = float(value)
    except OverflowError:
        return 0.0
    return number if math.isfinite(number) else 0.0


def _digest(token: str) -> str:
    return hashlib.sha256(token.encode("utf-8", "replace")).hexdigest()


def _cooling(until: float, secs: float, now: float) -> bool:
    """Whether a cool-off of ``secs`` seconds that ends at ``until`` still runs at ``now``. One that would end more than
    ``secs`` from now is over: the wall clock was set back, and the cool-off is not stretched with it."""
    return now < until <= now + secs


def toolkit_slug(toolkit: Any) -> Optional[str]:
    """``toolkit`` as a Composio toolkit slug (lowercase letters, digits and underscores), or None."""
    text = toolkit.strip().lower() if isinstance(toolkit, str) else ""
    return text if _SLUG.fullmatch(text) else None


def link_ok(url: Any) -> bool:
    """Whether ``url`` may go to the browser as a Connect Link: ``https`` on composio.dev or a subdomain of it
    (Composio documents ``https://connect.composio.dev/link/ln_…``), on the default port, with no user name or
    password, in printable ASCII with no space or backslash. A browser and Python then read the same host: a
    backslash, a user part, a percent sign or a letter outside ASCII in the host is how a link that reads as
    Composio's goes somewhere else. Never raises, whatever ``url`` holds: one Python cannot split is refused."""
    if (not isinstance(url, str) or not 0 < len(url) <= 4096 or not url.isascii() or not url.isprintable()
            or " " in url or "\\" in url):
        return False
    try:
        parts = urllib.parse.urlsplit(url)
    except ValueError:                                   # a bracket in the host that does not close or holds no IP
        return False
    if parts.scheme != "https" or "@" in parts.netloc:
        return False
    host, _, port = parts.netloc.lower().partition(":")
    if port not in ("", "443") or not _HOST.fullmatch(host):
        return False
    return host == "composio.dev" or host.endswith(".composio.dev")


def describe(tool: Mapping[str, Any]) -> str:
    """Composio's description of a meta tool as the model reads it: whole, less CATALOG_LINE, trailing spaces and
    runs of blank lines; the tool's title or name when it has none; clipped past DESC_MAX."""
    raw = tool.get("description")
    lines = [line.rstrip() for line in _text(raw).strip().splitlines() if not line.strip().startswith(CATALOG_LINE)]
    text = re.sub(r"\n{3,}", "\n\n", "\n".join(lines)).strip()
    if not text:
        title = tool.get("title")
        text = title.strip() if isinstance(title, str) and title.strip() else str(tool.get("name") or "")
    return text if len(text) <= DESC_MAX else text[: DESC_MAX - 1].rstrip() + "…"


def function_tool(tool: Mapping[str, Any]) -> dict[str, Any]:
    """One meta tool as a Responses-API function: its own name, ``describe``'s description, its input schema slimmed
    (apps_mcp.slim_schema). Not strict: Composio's schemas are not written for strict mode, which would make every
    optional argument required."""
    params = apps_mcp.slim_schema(tool.get("inputSchema") or {})
    if not isinstance(params, dict) or params.get("type") != "object":
        params = {"type": "object", "properties": {}}
    if not isinstance(params.setdefault("properties", {}), dict):
        params = {"type": "object", "properties": {}}        # a schema the model's API would refuse, whole turn and all
    return {"type": "function", "name": str(tool["name"]), "description": describe(tool), "parameters": params,
            "strict": False}


def offered(listed: Iterable[Any]) -> list[dict[str, Any]]:
    """The meta tools in a tools/list answer, as the brain offers them, in META_TOOLS order. The first of each name
    counts; any other tool is left out."""
    found: dict[str, Mapping[str, Any]] = {}
    for tool in listed:
        if isinstance(tool, Mapping) and tool.get("name") in META_TOOLS:
            found.setdefault(str(tool["name"]), tool)
    return [function_tool(found[name]) for name in META_TOOLS if name in found]


def _payload(result: Any) -> Optional[dict[str, Any]]:
    """Composio's own answer in a tools/call result: the structured content when it is an object, else the text when
    it is one JSON object. None for anything else: the text is then read as it is."""
    if not isinstance(result, Mapping):
        return None
    structured = result.get("structuredContent")
    if isinstance(structured, Mapping):
        return dict(structured)
    content = result.get("content")
    texts = [c["text"] for c in content if isinstance(c, Mapping) and c.get("type") == "text"
             and isinstance(c.get("text"), str)] if isinstance(content, list) else []
    if len(texts) != 1:
        return None
    try:
        data = json.loads(texts[0])
    except (ValueError, RecursionError):
        return None
    return data if isinstance(data, dict) else None


def _answer_object(result: Any) -> Optional[dict[str, Any]]:
    """Composio's own object, for Settings' bookkeeping (which apps are connected, a Connect Link): ``_payload``, or
    else the one text part that is a JSON object. Composio puts notes in parts of their own after its answer (seen
    2026-10-14: a custom-MCP hint and agent guidance after COMPOSIO_SEARCH_TOOLS's JSON), and those must not hide it.
    None when no part, or more than one, is an object. The model's reading stays ``_payload``'s: every part."""
    payload = _payload(result)
    if payload is not None or not isinstance(result, Mapping) or not isinstance(result.get("content"), list):
        return payload
    found: list[dict[str, Any]] = []
    for part in result["content"]:
        if not (isinstance(part, Mapping) and part.get("type") == "text" and isinstance(part.get("text"), str)):
            continue
        text = part["text"].strip()
        if not text.startswith("{"):
            continue
        try:
            data = json.loads(text)
        except (ValueError, RecursionError):
            continue
        if isinstance(data, dict):
            found.append(data)
    return found[0] if len(found) == 1 else None


def result_for_model(result: Any, payload: Optional[dict[str, Any]]) -> dict[str, Any]:
    """A tools/call result as the brain reads it, given its ``_payload``. When it carries Composio's own object
    (``data``, ``error``, ``successful``), that object with ``ok`` added: the shape composio_tools.ComposioBridge
    returns, so the model reads Composio alike in both eves, and the JSON is not sent back to it as one escaped
    string. Otherwise apps_mcp's reading (the text). Longer than apps_mcp.RESULT_CLIP, it is cut as a direct app's
    is."""
    if not isinstance(result, Mapping):
        return {"ok": False, "reason": "Composio sent no result"}
    if payload is None:
        content = result.get("content")
        return apps_mcp.result_for_model({**result, "content": content if isinstance(content, list) else []})
    error = payload.get("error")
    ok = (not result.get("isError") and payload.get("successful") is not False and payload.get("ok") is not False
          and not error)
    out: dict[str, Any] = {"ok": ok, **{k: v for k, v in payload.items() if k != "ok"}}
    if not ok and "reason" not in out:
        why = error if isinstance(error, str) else json.dumps(error, default=str) if error else ""
        out["reason"] = _clip(why or "Composio did not finish the call", 300)
    raw = json.dumps(out, ensure_ascii=False, default=str)
    if len(raw) <= apps_mcp.RESULT_CLIP:
        return out
    clipped: dict[str, Any] = {"ok": ok, "result": raw[: apps_mcp.RESULT_CLIP] + "…", "clipped": True}
    if "reason" in out:
        clipped["reason"] = out["reason"]
    return clipped


def _data(payload: Mapping[str, Any]) -> Mapping[str, Any]:
    data = payload.get("data")
    return data if isinstance(data, Mapping) else payload


def search_statuses(payload: Any) -> dict[str, bool]:
    """{toolkit: connected} from a COMPOSIO_SEARCH_TOOLS answer: ``data.toolkit_connection_statuses``, each item's
    ``toolkit`` and ``has_active_connection`` (Composio's docs, 2026-10-13). An item that does not read is skipped."""
    if not isinstance(payload, Mapping):
        return {}
    items = _data(payload).get("toolkit_connection_statuses")
    if isinstance(items, Mapping):                       # keyed by toolkit, should a version send it that way
        items = [{"toolkit": key, **value} for key, value in items.items() if isinstance(value, Mapping)]
    out: dict[str, bool] = {}
    for item in items if isinstance(items, list) else []:
        if isinstance(item, Mapping):
            slug, active = toolkit_slug(item.get("toolkit")), item.get("has_active_connection")
            if slug and isinstance(active, bool):
                out[slug] = active
    return out


def _manage_items(payload: Any) -> dict[str, Mapping[str, Any]]:
    """Each toolkit's part of a COMPOSIO_MANAGE_CONNECTIONS answer, by slug: ``data.results`` keyed by toolkit (the
    documented envelope), or a list of items that each name theirs."""
    if not isinstance(payload, Mapping):
        return {}
    results = _data(payload).get("results")
    out: dict[str, Mapping[str, Any]] = {}
    if isinstance(results, Mapping):
        for key, item in results.items():
            slug = toolkit_slug(key)
            if slug and isinstance(item, Mapping):
                out.setdefault(slug, item)
    elif isinstance(results, list):
        for item in results:
            if isinstance(item, Mapping):
                slug = toolkit_slug(item.get("toolkit") or item.get("toolkit_slug") or item.get("slug"))
                if slug:
                    out.setdefault(slug, item)
    return out


def _item_link(item: Mapping[str, Any]) -> Optional[str]:
    for key in _LINK_KEYS:
        value = item.get(key)
        if isinstance(value, str) and value.strip():
            return value.strip()
    return None


def _item_active(item: Mapping[str, Any]) -> Optional[bool]:
    """True when a toolkit's part says its connection is active, False when it says it is not (or brings a link to
    make one), None when it says neither."""
    for key in ("has_active_connection", "is_active", "connected"):
        if isinstance(item.get(key), bool):
            return bool(item[key])
    status = item.get("status") or item.get("connection_status")
    if isinstance(status, str) and status.strip():
        return status.strip().lower() in _ACTIVE
    return False if _item_link(item) else None


def _redirect_urls(value: Any, depth: int = 0) -> Iterator[str]:
    """Each ``redirect_url`` in ``value``, a few levels deep at most."""
    if depth > 6:
        return
    if isinstance(value, Mapping):
        found = value.get("redirect_url")
        if isinstance(found, str) and found.strip():
            yield found.strip()
        value = list(value.values())
    if isinstance(value, list):
        for item in value:
            if isinstance(item, (Mapping, list)):
                yield from _redirect_urls(item, depth + 1)


def _only_link(value: Any) -> Optional[str]:
    """The ``redirect_url`` in ``value``, at any depth, when there is one; None for none, and for two that differ:
    nothing then says which app each is for."""
    links: set[str] = set()
    for link in _redirect_urls(value):
        links.add(link)
        if len(links) > 1:
            return None
    return links.pop() if links else None


def connect_answer(payload: Any, slug: str) -> tuple[Optional[str], Optional[bool]]:
    """(link, connected) for the one toolkit a COMPOSIO_MANAGE_CONNECTIONS call asked about. ``connected`` is None
    when the answer does not say; the link is not checked here (link_ok). Only that toolkit's part speaks for it: an
    answer whose parts are all other apps' says nothing of it, and an answer with no parts gives a link only when it
    holds a single one."""
    if not isinstance(payload, Mapping):
        return None, None
    items = _manage_items(payload)
    item = items.get(slug)
    if item is None and len(items) == 1:                 # one toolkit was asked about: its part, whatever its key
        item = next(iter(items.values()))
    if item is None and items:                           # other apps' parts only: no link and no status of this one
        return None, None
    active = _item_active(item) if item is not None else None
    summary = _data(payload).get("summary")
    if active is None and isinstance(summary, Mapping) and summary.get("total_toolkits") == 1:
        active = True if summary.get("active_connections") == 1 else (
            False if summary.get("initiated_connections") == 1 else None)
    link = (_item_link(item) or _only_link(item)) if item is not None else _only_link(payload)
    return link, active


# -- the sign-in --
class ConnectTokenSource:
    """The Composio Connect sign-in in ``path`` (composio-connect.json, written by onboarding.ComposioConnectLogin).
    Called, it gives the access token, refreshed first when it has under REFRESH_EARLY_SECS left, or None when there
    is no usable sign-in. ``refresh(rejected)`` is the one refresh after Composio answers 401, and ``hold(rejected)``
    follows a 401 to the token that refresh handed out. Nothing here raises past a log line: a turn goes on without
    the apps."""

    def __init__(self, path: Path = CONNECT_TOKEN_FILE, *, transport: Optional[httpx.BaseTransport] = None,
                 clock: Callable[[], float] = time.time) -> None:
        self.path = Path(path).expanduser()
        self._transport = transport
        self._clock = clock
        self._lock = threading.Lock()
        self._refused = ""                   # SHA-256 of a refresh token the token endpoint refused: never sent again
        self._rejected = ""                  # SHA-256 of an access token Composio answered 401 to: never sent again
        self._retry_at = 0.0                 # no refresh before this, after one that failed for a reason that may pass
        self._held: Optional[tuple[str, float]] = None   # (SHA-256 of the token hold() set aside, the hold's end)
        self._unsaved: Optional[tuple[str, dict[str, Any]]] = None   # (refresh token it replaced, a sign-in not saved)

    def __repr__(self) -> str:
        return f"ConnectTokenSource({self.path.name})"

    def __call__(self) -> Optional[str]:
        with self._lock:
            try:
                return self._current(None)
            except Exception as e:  # noqa: BLE001 — the type only; a token is never logged
                log.warning("composio connect: the sign-in could not be read (%s)", type(e).__name__)
                return None

    def refresh(self, rejected: Optional[str] = None) -> Optional[str]:
        """After Composio refused ``rejected`` (HTTP 401): a new access token, or None. When the file already holds
        another token that has not lapsed (another call refreshed it meanwhile), that one, with no request. None
        while the sign-in is held (``hold``)."""
        with self._lock:
            try:
                return self._current(rejected or "")
            except Exception as e:  # noqa: BLE001 — the type only
                log.warning("composio connect: the sign-in could not be refreshed (%s)", type(e).__name__)
                return None

    def hold(self, rejected: Optional[str]) -> None:
        """Composio answered 401 to ``rejected`` too, the token ``refresh`` had just handed out. That may pass (S1:
        /mcp's 401 also covers Composio not finding the account), so for REFUSED_RETRY_SECS no token goes out and no
        refresh is asked for: a fresh pair would be refused as well. Then the same token is tried again, and after
        a 401 refreshed once more. A file that no longer holds ``rejected`` (the setup page signed in again
        meanwhile) is left alone: that sign-in gets its own try. A hold never asks for a new sign-in (needs_sign_in):
        that is for a sign-in that cannot be renewed."""
        with self._lock:
            try:
                access = _text(self._read().get("access_token"))
                if not rejected or access != rejected:
                    return
                self._held = (_digest(access), self._clock() + REFUSED_RETRY_SECS)
            except Exception as e:  # noqa: BLE001 — the type only
                log.warning("composio connect: the refused sign-in was not held (%s)", type(e).__name__)
                return
        log.warning("composio connect: Composio refused a token fresh from a refresh; trying the sign-in again in "
                    "%d s", REFUSED_RETRY_SECS)

    @property
    def needs_sign_in(self) -> bool:
        """Only a new sign-in helps: the token endpoint refused the file's refresh token, or the file holds neither
        an access token that has not lapsed (and that Composio did not refuse) nor a refresh token (or there is no
        file). A sign-in held after a 401 (``hold``) is not one of these: that may pass."""
        data = self._read()
        token = _text(data.get("refresh_token"))
        if token and _digest(token) == self._refused:
            return True
        access = _text(data.get("access_token"))
        live = bool(access) and _digest(access) != self._rejected and _expiry(data.get("expires_at")) > self._clock()
        return not (live or (token and _text(data.get("client_id"))))

    def _read(self) -> dict[str, Any]:
        """The file's sign-in, or {} when it holds none eve can use (missing, unreadable, another version or issuer).
        A refreshed sign-in that could not be saved stands in for the one it replaced."""
        data = _read_json(self.path)
        unsaved = self._unsaved
        if unsaved is not None:
            if _text(data.get("refresh_token")) == unsaved[0]:
                data = dict(unsaved[1])
            else:
                self._unsaved = None                     # the file moved on: a new sign-in, or a later save worked
        if data.get("version") != TOKEN_FILE_VERSION or data.get("issuer", CONNECT_ISSUER) != CONNECT_ISSUER:
            return {}
        return data

    def _current(self, rejected: Optional[str]) -> Optional[str]:
        """The token to send. ``rejected`` is None for an ordinary call, else the access token Composio just answered
        401 to ("" for none): it is never sent again, and another one that has not lapsed (a refresh by another call
        meanwhile) is used as it is. None, with no refresh, while the file's token is held (``hold``)."""
        data = self._read()
        if not data:
            return None
        access, now = _text(data.get("access_token")), self._clock()
        if self._holding(access, now):
            return None
        if rejected and access == rejected:
            self._rejected = _digest(access)
        if access and _digest(access) == self._rejected:
            access = ""
        expires = _expiry(data.get("expires_at"))
        if access and expires > now + (REFRESH_EARLY_SECS if rejected is None else 0.0):
            return access
        # Refresh. When that does not work, a token that has not lapsed still goes (an early refresh failed).
        return self._refresh(data) or (access if access and expires > now else None)

    def _holding(self, access: str, now: float) -> bool:
        """Whether ``access`` is the token hold() set aside and its hold still runs. A hold that is over, or whose
        token the file no longer holds (a new sign-in), is dropped."""
        held = self._held
        if held is None:
            return False
        if access and _digest(access) == held[0] and _cooling(held[1], REFUSED_RETRY_SECS, now):
            return True
        self._held = None
        return False

    def _refresh(self, data: dict[str, Any]) -> Optional[str]:
        """A new access token for the file's refresh token, saved with what came with it. None when there is no
        refresh token, Composio refused this one before, a failed refresh is still cooling off, or this one fails."""
        token, client_id = _text(data.get("refresh_token")), _text(data.get("client_id"))
        if (not token or not client_id or _digest(token) == self._refused
                or _cooling(self._retry_at, REFRESH_RETRY_SECS, self._clock())):
            return None
        form = {"grant_type": "refresh_token", "refresh_token": token, "client_id": client_id,
                "resource": CONNECT_RESOURCE}
        try:
            with httpx.Client(transport=self._transport, timeout=TOKEN_TIMEOUT_SECS) as client:
                response = client.post(CONNECT_TOKEN_URL, data=form, headers={"Accept": "application/json"})
        except httpx.HTTPError as e:
            return self._failed(type(e).__name__)
        try:
            answer = _answer(response)
        except RecursionError:                           # JSON nested past Python's limit: no answer at all
            answer = {}
        if response.status_code != 200:
            word = _oauth_word(answer)
            if response.status_code == 401 or word in _REFUSED:
                self._refused = _digest(token)
                log.warning("composio connect: Composio refused the sign-in (HTTP %d, %s); the owner signs in to "
                            "Composio again in Settings → Apps", response.status_code, word or "no error code")
                return None
            return self._failed(f"HTTP {response.status_code}, {word or 'no error code'}")
        access = answer.get("access_token")
        if not isinstance(access, str) or not _TOKEN_SHAPE.fullmatch(access):
            return self._failed("no usable access token in the answer")
        rotated = answer.get("refresh_token")
        rotated = rotated if isinstance(rotated, str) and _TOKEN_SHAPE.fullmatch(rotated) else token
        self._retry_at = 0.0
        self._save(token, access, rotated, answer)
        log.debug("composio connect: refreshed the sign-in")
        return access

    def _failed(self, why: str) -> None:
        self._retry_at = self._clock() + REFRESH_RETRY_SECS
        log.warning("composio connect: the sign-in was not refreshed (%s); trying again in %d s", why,
                    REFRESH_RETRY_SECS)
        return None

    def _save(self, used: str, access: str, rotated: str, answer: Mapping[str, Any]) -> None:
        """Write the refreshed sign-in back over the one it came from. A new sign-in written by the setup page while
        the refresh ran is kept instead; a refresh that cannot be written is held in memory (``_unsaved``)."""
        data = self._read()
        if _text(data.get("refresh_token")) != used:
            log.info("composio connect: a new sign-in arrived during a refresh; it is kept")
            return
        data.update(access_token=access, refresh_token=rotated,
                    expires_at=_expires_at(answer, access, self._clock()))
        scope = answer.get("scope")
        if isinstance(scope, str) and scope.strip():
            data["scope"] = scope
        try:
            write_private_json(self.path, data)
        except OSError as e:
            self._unsaved = (used, data)
            log.warning("composio connect: the refreshed sign-in was not saved (%s); it is kept until a restart",
                        type(e).__name__)
            return
        self._unsaved = None


# -- the bridge --
class _SignedOut(Exception):
    """No sign-in Composio takes: no token to send (none, or one held), no refresh to be had, or a 401 to the token
    fresh from one."""


class ConnectBridge:
    """The person's apps through Composio Connect, wrapped so nothing in here raises into the chat loop.
    ``token_source`` gives the access token (ConnectTokenSource); its ``refresh(rejected)``, when it has one, is
    called once after a 401, and its ``hold(rejected)`` after a 401 to the refreshed token. ``transport`` replaces
    the network (tests)."""

    def __init__(self, token_source: Callable[[], Optional[str]], *, url: str = MCP_URL,
                 transport: Optional[httpx.BaseTransport] = None, backoff: Sequence[float] = START_BACKOFF,
                 clock: Callable[[], float] = time.monotonic) -> None:
        self._tokens = token_source
        self._backoff = tuple(float(b) for b in backoff) or START_BACKOFF
        self._clock = clock
        self._local = threading.local()                  # per thread: the last status Composio sent, the token used
        self._https = [self._http(secs, transport) for secs in (CALL_TIMEOUT_SECS, OWN_TIMEOUT_SECS, OWN_TIMEOUT_SECS)]
        # One MCP session each (McpClient makes one call at a time), so a Connect Link never waits behind the brain's
        # three-minute workbench cell, nor behind Settings reading which apps are connected.
        self._brain = McpClient(url, self._token, http=self._https[0])
        self._linker = McpClient(url, self._token, http=self._https[1])
        self._looker = McpClient(url, self._token, http=self._https[2])
        self._tools: list[dict[str, Any]] = []
        self._names: frozenset[str] = frozenset()
        self._lock = threading.Lock()
        self._looking = threading.Lock()                 # one read of the connected apps at a time
        self._known: dict[str, bool] = {}                # toolkit -> connected, as last read or seen
        self._looked: dict[str, float] = {}              # toolkit -> when that was read
        self._look_retry_at = float("-inf")
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self._closed = False
        self._last_failure = ""

    def __repr__(self) -> str:
        return f"ConnectBridge({'up' if self._names else 'not up'}, {len(self._names)} tools)"

    def _http(self, read_secs: float, transport: Optional[httpx.BaseTransport]) -> httpx.Client:
        return httpx.Client(transport=transport, timeout=httpx.Timeout(read_secs, connect=CONNECT_TIMEOUT_SECS),
                            event_hooks={"response": [self._saw]})

    def _saw(self, response: httpx.Response) -> None:
        self._local.status = response.status_code

    def _token(self) -> Optional[str]:
        token = self._tokens()
        self._local.token = token
        return token

    # -- the duck type the brain reads (composio_tools.ComposioBridge) --
    @property
    def started(self) -> bool:
        return bool(self._names)

    @property
    def names(self) -> frozenset[str]:
        return self._names

    def tools(self) -> list[dict[str, Any]]:
        return [dict(tool) for tool in self._tools]

    def accounts_note(self) -> str:
        """Always "": Connect holds one person's own accounts, with no personal and other accounts to tell apart
        (the split full eve's accounts_note is for)."""
        return ""

    @property
    def needs_sign_in(self) -> bool:
        """Only a new sign-in helps (ConnectTokenSource.needs_sign_in), for the status page."""
        try:
            return bool(getattr(self._tokens, "needs_sign_in", False))
        except Exception:  # noqa: BLE001 — a token source that cannot say is taken as signed in
            return False

    def _why(self) -> str:
        """What the model reads for a call made without a sign-in Composio takes: sign in again when only that helps
        (needs_sign_in, so it says what Settings shows); else try again later (a refresh that failed for a reason
        that may pass, a sign-in held after Composio refused it fresh, or a new sign-in that arrived meanwhile)."""
        return SIGNED_OUT if self.needs_sign_in else NOT_RENEWED

    # -- starting --
    def start(self) -> None:
        """List the tools on a thread, trying again with backoff (START_BACKOFF, its last step repeating) until it
        works or ``close()``. Returns at once; a call while that thread runs, or once the tools are up, does nothing."""
        with self._lock:
            if self._names or self._closed or (self._thread is not None and self._thread.is_alive()):
                return
            self._thread = threading.Thread(target=self._keep_starting, name="composio-connect", daemon=True)
            self._thread.start()

    def _keep_starting(self) -> None:
        for attempt in itertools.count():
            if self._stop.is_set() or self.try_start():
                return
            if self._stop.wait(self._backoff[min(attempt, len(self._backoff) - 1)]):
                return

    def try_start(self) -> bool:
        """One attempt to list Composio's tools; True once the meta tools are up. Never raises."""
        if self._names:
            return True
        try:
            listed = self._authorized(self._brain, lambda client: client.list_tools())
            tools = offered(listed)
        except _SignedOut:
            self._failed("signed out" if self.needs_sign_in else "the sign-in was not renewed")
            return False
        except Exception as e:  # noqa: BLE001 — the type only
            self._failed(type(e).__name__)
            return False
        if not tools:
            self._failed(f"none of the meta tools among {len(listed)} listed")
            return False
        self._tools = tools
        self._names = frozenset(tool["name"] for tool in tools)
        others = [n for n in (str(t.get("name")) for t in listed if isinstance(t, Mapping)) if n not in self._names]
        shown = [n for n in others if re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", n)][:10]
        log.info("composio connect: up, %d tools%s", len(tools),
                 f"; left out {len(others)} others ({', '.join(shown)})" if others else "")
        return True

    def _failed(self, why: str) -> None:
        """One line per failed start: a warning when the reason is new, else a quieter repeat; none once closing."""
        if self._stop.is_set():
            return
        level = logging.INFO if why == self._last_failure else logging.WARNING
        self._last_failure = why
        log.log(level, "composio connect: the apps are not up yet (%s)", why)

    def close(self) -> None:
        """Stop the start thread and close the connections. The bridge is not used after this."""
        with self._lock:
            self._closed = True
            thread = self._thread
        self._stop.set()
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout=2.0)
        for http in self._https:
            try:
                http.close()
            except Exception as e:  # noqa: BLE001 — closing is best effort
                log.debug("composio connect: a connection did not close (%s)", type(e).__name__)

    # -- calls --
    def _authorized(self, client: McpClient, call: Callable[[McpClient], _T]) -> _T:
        """``call(client)`` with the sign-in. When Composio answers 401, the sign-in is refreshed once and the call
        made once more. A 401 to that fresh token too holds the sign-in (the token source's ``hold``): until the
        hold is over, no call sends it or mints another. _SignedOut when there is no token to send, no refresh, or a
        second 401. McpClient raises the same McpError for 401 and 403, so the status is read off the response itself
        (``_saw``, per thread), as is the token that call sent (``_token``)."""
        if not self._token():
            raise _SignedOut()
        for attempt in (1, 2):
            self._local.status = None
            try:
                return call(client)
            except McpError:
                if getattr(self._local, "status", None) != 401:
                    raise
                if attempt == 2:
                    hold = getattr(self._tokens, "hold", None)
                    if hold is not None:
                        hold(getattr(self._local, "token", None))
                    raise _SignedOut() from None
            refresh = getattr(self._tokens, "refresh", None)
            if refresh is None or not refresh(getattr(self._local, "token", None)):
                raise _SignedOut()
        raise AssertionError("unreachable")

    def execute(self, name: str, args: Mapping[str, Any]) -> dict[str, Any]:
        """Blocking. Never raises: a failure is a dict the model can read and tell the owner about."""
        if not isinstance(name, str) or name not in self._names:
            return {"ok": False, "reason": f"{name} is not available" if self._names else NOT_UP}
        if args is not None and not isinstance(args, Mapping):
            return {"ok": False, "reason": "the arguments must be a JSON object"}
        arguments = dict(args or {})
        try:
            result = self._authorized(self._brain, lambda client: client.call_tool(name, arguments))
        except _SignedOut:
            log.warning("composio connect: %s not run: no sign-in Composio takes", name)
            return {"ok": False, "reason": self._why()}
        except Exception as e:  # noqa: BLE001 — the type only: arguments may hold mail text
            log.warning("composio connect: %s failed: %s", name, type(e).__name__)
            reason = f"{type(e).__name__}: {_clip(e, 200)}"
            return {"ok": False, "reason": reason + (MAY_HAVE_RUN if isinstance(e, _MAY_HAVE_RUN) else "")}
        # The call has run. Nothing below may turn its answer into a failure the model would run again.
        try:
            payload = _payload(result)
            out = result_for_model(result, payload)
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("composio connect: %s ran; its answer was not read (%s)", name, type(e).__name__)
            return {"ok": False, "reason": UNREADABLE}
        seen = _answer_object(result)
        if seen is not None:
            try:
                self._observe(name, seen)
            except Exception as e:  # noqa: BLE001 — Settings' bookkeeping never costs the model its answer
                log.info("composio connect: %s's answer was not read for Settings (%s)", name, type(e).__name__)
        return out

    # -- Settings → Apps --
    def connect_link(self, toolkit: str) -> Optional[str]:
        """The Connect Link that connects ``toolkit`` (a Composio toolkit slug, as Settings → Apps names it), or None:
        already connected, not a toolkit eve connects (a malformed slug, or one of the web toolkits eve never runs),
        Composio not reachable, or a link that is not on composio.dev (link_ok). Asks COMPOSIO_MANAGE_CONNECTIONS in
        the one shape that only checks or links (composio_tools.manage_needs_yes). Never raises."""
        slug = toolkit_slug(toolkit)
        if slug is None or slug in composio_tools.WEB_TOOLKITS:
            return None
        try:
            result = self._authorized(self._linker, lambda client: client.call_tool(MANAGE_CONNECTIONS,
                                                                                    {"toolkits": [slug]}))
            link, active = connect_answer(_answer_object(result), slug)
        except _SignedOut:
            log.warning("composio connect: no Connect Link for %s: no sign-in Composio takes", slug)
            return None
        except Exception as e:  # noqa: BLE001 — the type only
            log.warning("composio connect: no Connect Link for %s (%s)", slug, type(e).__name__)
            return None
        if active:
            self._remember({slug: True}, fresh=True)
            log.info("composio connect: %s is already connected", slug)
            return None
        if link is None:
            log.warning("composio connect: Composio sent no Connect Link for %s", slug)
            return None
        if not link_ok(link):
            log.warning("composio connect: the link Composio sent for %s is not on composio.dev; not used", slug)
            return None
        self._remember({slug: False}, fresh=False)       # being connected now: read it again next time
        return link

    def connections(self, toolkits: Iterable[str] = tuple(APPS), *,
                    max_age: float = CONNECTIONS_TTL_SECS) -> dict[str, bool]:
        """Which of ``toolkits`` the person has connected in Composio: {slug: connected}, in their order. Read from
        COMPOSIO_SEARCH_TOOLS (it only looks), for the apps not read in the last ``max_age`` seconds (0: all, now),
        and from each search and connection check the brain runs. An app Composio did not report, or any app while
        Composio cannot be reached, keeps what was last known: False when nothing was. Never raises."""
        if isinstance(toolkits, str):
            toolkits = (toolkits,)
        try:
            slugs = [s for s in dict.fromkeys(toolkit_slug(t) for t in toolkits) if s]
        except TypeError:                                 # not a list of names
            slugs = []
        if slugs:
            with self._looking:                          # a caller that waited here finds the answer fresh
                stale = self._stale(slugs, max_age)
                for start in range(0, len(stale), LOOK_MAX):
                    if not self._look(stale[start:start + LOOK_MAX]):
                        break
        with self._lock:
            return {slug: self._known.get(slug, False) for slug in slugs}

    def _stale(self, slugs: list[str], max_age: float) -> list[str]:
        with self._lock:
            now = self._clock()
            if now < self._look_retry_at:
                return []
            return [s for s in slugs if now - self._looked.get(s, float("-inf")) >= max_age]

    def _look(self, slugs: list[str]) -> bool:
        """Read whether ``slugs`` are connected, in one COMPOSIO_SEARCH_TOOLS call; False when it failed."""
        queries = [{"use_case": _LOOKS.get(slug) or f"list my recent items in {slug}"} for slug in slugs]
        try:
            result = self._authorized(self._looker, lambda client: client.call_tool(
                SEARCH_TOOLS, {"queries": queries, "session": {"generate_id": True}}))
            seen = search_statuses(_answer_object(result))
        except Exception as e:  # noqa: BLE001 — the type only
            with self._lock:
                self._look_retry_at = self._clock() + LOOK_RETRY_SECS
            log.warning("composio connect: the connected apps were not read (%s)",
                        "signed out" if isinstance(e, _SignedOut) else type(e).__name__)
            return False
        with self._lock:
            now = self._clock()
            self._known.update(seen)
            for slug in slugs:
                self._looked[slug] = now                 # read, even when Composio did not report it
        log.info("composio connect: %d of %d apps read as connected", sum(1 for s in slugs if seen.get(s)),
                 len(slugs))
        return True

    def _remember(self, seen: Mapping[str, bool], *, fresh: bool) -> None:
        """Record what was learned of some apps. ``fresh``: it is their state now, so connections() trusts it for
        CONNECTIONS_TTL_SECS; otherwise it is about to change (a link was just sent), so they are read next time."""
        with self._lock:
            now = self._clock()
            for slug, connected in seen.items():
                self._known[slug] = connected
                if fresh:
                    self._looked[slug] = now
                else:
                    self._looked.pop(slug, None)

    def _observe(self, name: str, payload: Mapping[str, Any]) -> None:
        """What a search or a connection check the brain ran says about which apps are connected (connections())."""
        if name == SEARCH_TOOLS:
            self._remember(search_statuses(payload), fresh=True)
        elif name == MANAGE_CONNECTIONS:
            for slug, item in _manage_items(payload).items():
                active = _item_active(item)
                if active is not None:
                    self._remember({slug: active}, fresh=active)
