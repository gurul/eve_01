"""Updates' Composio sources: what needs the owner in the apps connected through Composio (docs/updates.md, Sources).

Owner, 2026-10-16: "make sure updates is also linked to all the composio stuff". So the Updates pane also reads, one
source each: Slack (direct messages and mentions of the owner in the last 3 days that the owner has not answered),
Notion (pages edited in the last 14 days that hold an open to-do, mention the owner, or are assigned to the owner and
still open), Linear (open issues assigned to the owner, due within 14 days or updated in the last 7), and the
owner's OTHER Gmail and Google Calendar accounts (composio_tools.OTHER_ACCOUNT_TOOLKITS: the work inbox of the last
7 days without promotions and social, and the work calendar of the next 7 days).

How a read goes:

* ``Reader`` is one gather's view of Composio: the owner's connected accounts are listed once, lazily
  (``ComposioBridge.connected_accounts``), and every source reads through ``Reader.call``.
* ``Reader.call`` runs only a slug in ``READ_SLUGS``, an allow-list of exact slugs that only read, and checks each one
  again with composio_tools' own ``consequential_slugs`` and ``is_read_only`` before it reaches
  ``ComposioBridge.read_tool`` (which refuses a writing slug a third time). Nothing here can send, change or delete.
* A source is **off** when its toolkit has no account the owner connected (a sign-in started and not finished counts
  as none). It **failed** when Composio could not be read, or when its only connection has expired or failed: an
  expired connection is a lost source, not an empty one, so ``Updates._rebuild`` keeps its old cards and tries again.
* Each item is ``updates.item``, clipped like the others, with a stable id (``REF_PREFIX``) and, when the app gave
  one, its own ``link`` (``safe_link``: https on that app's host only). The link is never shown to the model.

Logs carry counts, kinds and exception types only, never a title, a sender, an id or an alias.
"""
from __future__ import annotations

import logging
import os
import re
import threading
import time
import urllib.parse
from datetime import datetime, timedelta, timezone
from typing import Any, Callable, Mapping, Optional

from . import composio_tools
from .updates import MAIL_QUERY, _one_line, event_item, item

log = logging.getLogger(__name__)

ENV_SWITCH = "EVE_UPDATES_COMPOSIO"

# The source kinds, each one app; and the ref prefix of each (``updates.REF_KINDS``).
KINDS: tuple[str, ...] = ("slack", "notion", "linear", "work_email", "work_calendar")
REF_PREFIX: dict[str, str] = {"slack": "slack", "notion": "notion", "linear": "linear", "work_email": "wgmail",
                              "work_calendar": "wcal"}
TOOLKIT: dict[str, str] = {"slack": "slack", "notion": "notion", "linear": "linear", "work_email": "gmail",
                           "work_calendar": "googlecalendar"}
# The time limit of each source (updates.gather), in seconds. Notion reads a page's blocks one call at a time.
LIMITS: dict[str, float] = {"slack": 30.0, "notion": 45.0, "linear": 25.0, "work_email": 45.0, "work_calendar": 30.0}

# Every slug this module may run. Each one only reads (tests check them against composio_tools.consequential_slugs).
READ_SLUGS: frozenset[str] = frozenset({
    "SLACK_SEARCH_MESSAGES",
    "NOTION_GET_ABOUT_ME", "NOTION_SEARCH_NOTION_PAGE", "NOTION_FETCH_BLOCK_CONTENTS",
    "LINEAR_LIST_LINEAR_ISSUES",
    "GMAIL_FETCH_EMAILS",
    "GOOGLECALENDAR_EVENTS_LIST",
})

# The hosts an item's link may point at, by kind (the app's own pages; a subdomain counts).
LINK_HOSTS: dict[str, tuple[str, ...]] = {"slack": ("slack.com",), "notion": ("notion.so", "notion.com", "notion.site"),
                                          "linear": ("linear.app",), "work_email": ("google.com",),
                                          "work_calendar": ("google.com",)}

SLACK_DAYS = 3
SLACK_COUNT = 50                         # per search; the owner's own messages: 100
SLACK_MAX = 15                           # conversations
NOTION_DAYS = 14
NOTION_SEARCH = 50                       # pages read from the search, the most recently edited first
NOTION_PAGES = 10                        # pages whose blocks are read
NOTION_MAX = 15
NOTION_BLOCKS = 100
LINEAR_FIRST = 100
LINEAR_DUE_DAYS = 14
LINEAR_UPDATED_DAYS = 7
LINEAR_MAX = 15
WORK_MAIL_MAX = 30
WORK_CALENDAR_DAYS = 7
MAX_ACCOUNTS = 3                         # accounts read per kind

ACTIVE = "ACTIVE"
LOST_STATES = frozenset({"EXPIRED", "FAILED", "INACTIVE"})      # a connection that was made and no longer works
DONE_WORDS = frozenset({"done", "complete", "completed", "closed", "canceled", "cancelled", "duplicate", "shipped",
                        "resolved", "archived", "won't do", "wont do", "not doing"})
PRIORITY = {1: "Urgent", 2: "High", 3: "Medium", 4: "Low"}

_SLACK_ID = re.compile(r"^[A-Z0-9]{3,32}$")
_SLACK_TS = re.compile(r"^\d{9,12}\.\d{1,8}$")
_SAFE_ID = re.compile(r"^[A-Za-z0-9_.:-]{1,128}$")
_ALIAS = re.compile(r"[^a-z0-9_-]+")


class SourceOff(FileNotFoundError):
    """The toolkit has no connected account: the source is off (updates.gather reads FileNotFoundError as off)."""


class ReadFailed(RuntimeError):
    """Composio could not be read for this source, or its connection has expired: the source failed (and is lost)."""


def enabled(environ: Optional[Mapping[str, str]] = None) -> bool:
    """On unless EVE_UPDATES_COMPOSIO is 0/false/no/off."""
    env = os.environ if environ is None else environ
    return (env.get(ENV_SWITCH) or "1").strip().lower() not in ("0", "false", "no", "off")


def usable(bridge: Any) -> bool:
    """A bridge these sources can read through: full eve's ComposioBridge once its client is made."""
    return (bridge is not None and bool(getattr(bridge, "can_read", False))
            and callable(getattr(bridge, "read_tool", None)) and callable(getattr(bridge, "connected_accounts", None)))


def safe_link(url: Any, kind: Optional[str] = None) -> Optional[str]:
    """``url`` when it is https on one of ``kind``'s hosts (LINK_HOSTS; any kind's for None), with no user part, in
    printable ASCII; else None."""
    if not isinstance(url, str) or not 0 < len(url) <= 2048 or not url.isascii() or not url.isprintable() \
            or " " in url or "\\" in url:
        return None
    try:
        parts = urllib.parse.urlsplit(url)
    except ValueError:
        return None
    host = (parts.hostname or "").lower()
    if parts.scheme != "https" or "@" in parts.netloc or parts.port not in (None, 443):
        return None
    hosts = LINK_HOSTS.get(kind, ()) if kind is not None else tuple(h for hs in LINK_HOSTS.values() for h in hs)
    return url if any(host == h or host.endswith("." + h) for h in hosts) else None


def _linked(row: dict[str, Any], url: Any, kind: str) -> dict[str, Any]:
    link = safe_link(url, kind)
    if link:
        row["link"] = link
    return row


def alias_word(alias: str) -> str:
    """An account's alias as it goes in a ref (``wgmail:<alias>/<thread>``): lowercase letters, digits, - and _."""
    return _ALIAS.sub("-", alias.strip().lower()).strip("-")[:32] or "other"


# ---- the reader ------------------------------------------------------------------------------------------------

class Reader:
    """One gather's reads through ``bridge`` (composio_tools.ComposioBridge). The connected accounts are listed once,
    by the first source that asks; the others wait for that answer."""

    def __init__(self, bridge: Any) -> None:
        self.bridge = bridge
        self._lock = threading.Lock()
        self._accounts: Optional[list[dict[str, str]]] = None
        self._error: Optional[str] = None
        self.calls: list[str] = []           # the slugs run, in order (tests)

    def accounts(self) -> list[dict[str, str]]:
        with self._lock:
            if self._accounts is None and self._error is None:
                try:
                    got = self.bridge.connected_accounts()
                    self._accounts = [a for a in got if isinstance(a, Mapping)] if isinstance(got, list) else []
                except Exception as e:  # noqa: BLE001 — the type only
                    self._error = type(e).__name__
            if self._error is not None:
                raise ReadFailed(f"the connected accounts could not be listed ({self._error})")
            return list(self._accounts or [])

    def accounts_for(self, kind: str) -> list[tuple[str, str]]:
        """``[(alias, id)]`` of the accounts ``kind`` reads, at most MAX_ACCOUNTS. For the work kinds only accounts
        aliased other than the owner's own (composio_tools.PRIMARY_ALIAS): the personal one is read directly. Raises
        SourceOff when there is none, ReadFailed when the only ones have expired."""
        toolkit = TOOLKIT[kind]
        rows = [a for a in self.accounts() if str(a.get("toolkit") or "").lower() == toolkit and a.get("id")]
        if toolkit in composio_tools.OTHER_ACCOUNT_TOOLKITS:
            rows = [a for a in rows
                    if str(a.get("alias") or "").strip().lower() not in ("", composio_tools.PRIMARY_ALIAS)]
        live = [(str(a.get("alias") or ""), str(a["id"])) for a in rows if str(a.get("status") or "") == ACTIVE]
        if live:
            return live[:MAX_ACCOUNTS]
        if any(str(a.get("status") or "").upper() in LOST_STATES for a in rows):
            raise ReadFailed(f"the {toolkit} connection has expired")
        raise SourceOff(f"{toolkit} is not connected")

    def call(self, slug: str, arguments: Mapping[str, Any], account_id: str) -> dict[str, Any]:
        """Run one allowed read → its ``data``. Raises PermissionError for a slug off the allow-list or one
        composio_tools says writes, ReadFailed when the call failed."""
        if slug not in READ_SLUGS:
            raise PermissionError(f"{slug} is not one of Updates' reads")
        tools = {"tools": [{"tool_slug": slug, "arguments": dict(arguments)}]}
        if not composio_tools.is_read_only(slug) or composio_tools.consequential_slugs(composio_tools.MULTI_EXECUTE,
                                                                                        tools):
            raise PermissionError(f"{slug} is not a read")
        self.calls.append(slug)
        got = self.bridge.read_tool(slug, dict(arguments), account_id)
        if not isinstance(got, Mapping) or got.get("ok") is False:
            raise ReadFailed(f"{slug} failed")
        data = got.get("data")
        if not isinstance(data, Mapping):
            raise ReadFailed(f"{slug} sent no data")
        return dict(data)


# ---- small readers ---------------------------------------------------------------------------------------------

def _iso(value: Any) -> str:
    """A timestamp as ISO 8601: Slack's ``ts`` (epoch seconds), or an ISO string as it is."""
    if isinstance(value, (int, float)) and not isinstance(value, bool):
        return datetime.fromtimestamp(float(value), tz=timezone.utc).astimezone().isoformat(timespec="seconds")
    text = _one_line(value)
    if re.fullmatch(r"\d{9,12}(?:\.\d+)?", text):
        return _iso(float(text))
    return text[:40]


def _parse(value: Any) -> Optional[datetime]:
    text = _one_line(value)
    if not text:
        return None
    try:
        when = datetime.fromisoformat(text.replace("Z", "+00:00"))
    except ValueError:
        return None
    return when if when.tzinfo else when.replace(tzinfo=timezone.utc)


def _day(value: Any) -> Optional[str]:
    text = _one_line(value)[:10]
    return text if re.fullmatch(r"\d{4}-\d{2}-\d{2}", text) else None


# ---- Slack -----------------------------------------------------------------------------------------------------

_SLACK_USER = re.compile(r"<@([A-Z0-9]+)(?:\|[^>]*)?>")
_SLACK_CHANNEL = re.compile(r"<#[A-Z0-9]+\|([^>]*)>")
_SLACK_LINK = re.compile(r"<((?:https?|mailto):[^>|]+)(?:\|([^>]*))?>")
_SLACK_SPECIAL = re.compile(r"<!(here|channel|everyone)[^>]*>")


def slack_text(text: Any, owner: Optional[str]) -> str:
    """Slack's markup as plain words: the owner's mention as "@you", another's as "@someone", a channel by its name,
    a link by its label (or "(link)")."""
    s = str(text or "")
    s = _SLACK_USER.sub(lambda m: "@you" if owner and m.group(1) == owner else "@someone", s)
    s = _SLACK_CHANNEL.sub(lambda m: "#" + m.group(1), s)
    s = _SLACK_LINK.sub(lambda m: m.group(2) or "(link)", s)
    s = _SLACK_SPECIAL.sub(lambda m: "@" + m.group(1), s)
    return s.replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&")


def _matches(data: Mapping[str, Any]) -> list[dict[str, Any]]:
    msgs = data.get("messages")
    got = msgs.get("matches") if isinstance(msgs, Mapping) else None
    return [m for m in got if isinstance(m, dict)] if isinstance(got, list) else []


def _slack_ok(m: Mapping[str, Any]) -> bool:
    ch = m.get("channel")
    return (isinstance(ch, Mapping) and isinstance(ch.get("id"), str) and bool(_SLACK_ID.match(ch["id"]))
            and isinstance(m.get("ts"), str) and bool(_SLACK_TS.match(m["ts"])))


def slack_items(reader: Reader, now: datetime) -> list[dict[str, Any]]:
    """Blocking. Direct messages and mentions of the owner from the last SLACK_DAYS days that the owner has not
    answered since (no message of theirs in that conversation after it), one item per conversation, newest first:
    ``slack:<channel>/<ts of its newest message>``."""
    since = now - timedelta(days=SLACK_DAYS)
    after = (since.date() - timedelta(days=1)).isoformat()             # Slack's after: leaves its own day out
    out: list[dict[str, Any]] = []
    for _alias, account in reader.accounts_for("slack"):
        search = {"count": SLACK_COUNT, "sort": "timestamp", "sort_dir": "desc"}
        mine = _matches(reader.call("SLACK_SEARCH_MESSAGES", {**search, "query": "from:me", "count": 100}, account))
        owner = next((m["user"] for m in mine if isinstance(m.get("user"), str) and m["user"]), None)
        last_mine: dict[str, float] = {}
        for m in mine:
            if _slack_ok(m):
                ch = m["channel"]["id"]
                last_mine[ch] = max(last_mine.get(ch, 0.0), float(m["ts"]))
        found = [m for m in _matches(reader.call("SLACK_SEARCH_MESSAGES", {**search, "query": f"is:dm after:{after}"},
                                                 account))
                 if isinstance(m.get("channel"), Mapping) and (m["channel"].get("is_im") or m["channel"].get("is_mpim"))]
        if owner and _SLACK_ID.match(owner):
            found += _matches(reader.call("SLACK_SEARCH_MESSAGES", {**search, "query": f"<@{owner}> after:{after}"},
                                          account))
        convs: dict[str, list[dict[str, Any]]] = {}
        seen: set[tuple[str, str]] = set()
        for m in found:
            if not _slack_ok(m) or (owner and m.get("user") == owner):
                continue
            ch, ts = m["channel"]["id"], m["ts"]
            if (ch, ts) in seen or float(ts) < since.timestamp() or last_mine.get(ch, 0.0) > float(ts):
                continue
            seen.add((ch, ts))
            convs.setdefault(ch, []).append(m)
        for ch, msgs in convs.items():
            msgs.sort(key=lambda m: float(m["ts"]))
            newest = msgs[-1]
            channel = newest["channel"]
            who = _one_line(newest.get("username") or "someone")
            if channel.get("is_im"):
                title = f"Slack: direct message from {who}"
            elif channel.get("is_mpim"):
                title = f"Slack: group message from {who}"
            else:
                title = f"Slack: mention in #{_one_line(channel.get('name')) or 'a channel'} from {who}"
            lines = [f"{len(msgs)} message{'s' if len(msgs) != 1 else ''} the owner has not answered."]
            lines += [f"{_one_line(m.get('username') or 'someone')}: {slack_text(m.get('text'), owner)}"
                      for m in msgs[-5:]]
            row = item(f"slack:{ch}/{newest['ts']}", "slack", title, "\n".join(lines), sender=who,
                       when=_iso(newest["ts"]))
            out.append(_linked(row, newest.get("permalink"), "slack"))
    out.sort(key=lambda r: r.get("when") or "", reverse=True)
    return out[:SLACK_MAX]


# ---- Notion ----------------------------------------------------------------------------------------------------

def _plain(rich: Any) -> str:
    if not isinstance(rich, list):
        return ""
    return "".join(str(r.get("plain_text") or "") for r in rich if isinstance(r, Mapping))


def _mentions(rich: Any, owner: Optional[str]) -> bool:
    if not owner or not isinstance(rich, list):
        return False
    for r in rich:
        mention = r.get("mention") if isinstance(r, Mapping) else None
        user = mention.get("user") if isinstance(mention, Mapping) else None
        if isinstance(user, Mapping) and user.get("id") == owner:
            return True
    return False


def notion_page(page: Mapping[str, Any], owner: Optional[str]) -> dict[str, Any]:
    """What a page's properties say: its title, whether it is assigned to the owner (a people property holding
    them), its status (a status or select property named Status or State), whether that reads as done (or a checkbox
    named Done is ticked), and its first date."""
    props = page.get("properties") if isinstance(page.get("properties"), Mapping) else {}
    title, status, due, assigned, done = "", "", None, False, False
    for name, prop in props.items():
        if not isinstance(prop, Mapping):
            continue
        kind = prop.get("type")
        if kind == "title":
            title = title or _plain(prop.get("title"))
        elif kind == "people" and owner:
            assigned = assigned or any(isinstance(p, Mapping) and p.get("id") == owner for p in prop.get("people") or [])
        elif kind in ("status", "select") and str(name).strip().lower() in ("status", "state"):
            value = prop.get(kind)
            if isinstance(value, Mapping) and value.get("name"):
                status = _one_line(value["name"])
                done = done or status.lower() in DONE_WORDS
        elif kind == "checkbox" and str(name).strip().lower() in ("done", "complete", "completed"):
            done = done or prop.get("checkbox") is True
        elif kind == "date" and due is None:
            value = prop.get("date")
            due = _day(value.get("start")) if isinstance(value, Mapping) else None
    return {"title": title, "status": status, "due": due, "assigned": assigned, "done": done}


def notion_blocks(data: Mapping[str, Any], owner: Optional[str]) -> tuple[list[str], list[str]]:
    """(open to-dos, lines that mention the owner) among a page's first-level blocks."""
    todos, mentions = [], []
    for block in data.get("results") or []:
        if not isinstance(block, Mapping) or block.get("archived") or block.get("in_trash"):
            continue
        kind = block.get("type")
        body = block.get(kind) if isinstance(kind, str) else None
        if not isinstance(body, Mapping):
            continue
        rich = body.get("rich_text")
        if kind == "to_do" and body.get("checked") is False and _plain(rich).strip():
            todos.append(_one_line(_plain(rich)))
        elif _mentions(rich, owner):
            mentions.append(_one_line(_plain(rich)))
    return todos, mentions


def notion_items(reader: Reader, now: datetime, deadline: Optional[float] = None,
                 clock: Callable[[], float] = time.monotonic) -> list[dict[str, Any]]:
    """Blocking. Pages edited in the last NOTION_DAYS days that hold an open to-do, mention the owner, or are
    assigned to the owner and not done; most recently edited first: ``notion:<page>``. The blocks of at most
    NOTION_PAGES pages are read, and none after ``deadline`` on ``clock``."""
    since = now - timedelta(days=NOTION_DAYS)
    out: list[dict[str, Any]] = []
    for _alias, account in reader.accounts_for("notion"):
        about = reader.call("NOTION_GET_ABOUT_ME", {}, account)
        bot = about.get("bot") if isinstance(about.get("bot"), Mapping) else {}
        owner_user = (bot.get("owner") or {}).get("user") if isinstance(bot.get("owner"), Mapping) else None
        owner = owner_user.get("id") if isinstance(owner_user, Mapping) and isinstance(owner_user.get("id"), str) \
            else None
        found = reader.call("NOTION_SEARCH_NOTION_PAGE", {
            "query": "", "timestamp": "last_edited_time", "direction": "descending", "page_size": NOTION_SEARCH,
            "filter_value": "page", "filter_property": "object"}, account)
        pages = [p for p in found.get("results") or [] if isinstance(p, Mapping) and p.get("object") == "page"
                 and not p.get("archived") and not p.get("in_trash") and isinstance(p.get("id"), str)
                 and _SAFE_ID.match(p["id"]) and (_parse(p.get("last_edited_time")) or since) > since]
        read = 0
        for page in pages:
            facts = notion_page(page, owner)
            if facts["done"]:
                continue
            todos: list[str] = []
            mentions: list[str] = []
            if read < NOTION_PAGES and (deadline is None or clock() < deadline):
                read += 1
                todos, mentions = notion_blocks(reader.call("NOTION_FETCH_BLOCK_CONTENTS", {
                    "block_id": page["id"], "page_size": NOTION_BLOCKS}, account), owner)
            if not (todos or mentions or facts["assigned"]):
                continue
            lines = []
            if facts["assigned"]:
                lines.append("Assigned to the owner." + (f" Status: {facts['status']}." if facts["status"] else ""))
            elif facts["status"]:
                lines.append(f"Status: {facts['status']}.")
            if facts["due"]:
                lines.append(f"Date: {facts['due']}.")
            lines += [f"- [ ] {t}" for t in todos]
            lines += [f"Mentions the owner: {m}" for m in mentions]
            row = item(f"notion:{page['id']}", "notion", f"Notion: {facts['title'] or 'Untitled'}", "\n".join(lines),
                       when=_iso(page.get("last_edited_time")))
            out.append(_linked(row, page.get("url"), "notion"))
            if len(out) >= NOTION_MAX:
                return out
    return out


# ---- Linear ----------------------------------------------------------------------------------------------------

def linear_items(reader: Reader, now: datetime) -> list[dict[str, Any]]:
    """Blocking. Open issues assigned to the owner that are due within LINEAR_DUE_DAYS days (or overdue) or were
    updated in the last LINEAR_UPDATED_DAYS days; the nearest due date first, then the latest update:
    ``linear:<issue id>``."""
    due_by = (now + timedelta(days=LINEAR_DUE_DAYS)).date().isoformat()
    updated_since = now - timedelta(days=LINEAR_UPDATED_DAYS)
    picked: list[tuple[tuple, dict[str, Any]]] = []
    for _alias, account in reader.accounts_for("linear"):
        data = reader.call("LINEAR_LIST_LINEAR_ISSUES", {"assignee_id": "me", "first": LINEAR_FIRST}, account)
        for issue in data.get("issues") or []:
            if not isinstance(issue, Mapping) or not isinstance(issue.get("id"), str) or not _SAFE_ID.match(issue["id"]):
                continue
            state = _one_line((issue.get("state") or {}).get("name") if isinstance(issue.get("state"), Mapping) else "")
            if state.lower() in DONE_WORDS:
                continue
            due = _day(issue.get("dueDate"))
            updated = _parse(issue.get("updatedAt"))
            if not ((due and due <= due_by) or (updated and updated >= updated_since)):
                continue
            ident = _one_line(issue.get("identifier"))
            lines = [f"State: {state}." if state else ""]
            if isinstance(issue.get("priority"), int) and issue["priority"] in PRIORITY:
                lines.append(f"Priority: {PRIORITY[issue['priority']]}.")
            if due:
                lines.append(f"Due: {due}.")
            for key, label in (("project", "Project"), ("team", "Team")):
                value = issue.get(key)
                if isinstance(value, Mapping) and value.get("name"):
                    lines.append(f"{label}: {_one_line(value['name'])}.")
            if updated:
                lines.append(f"Updated: {updated.date().isoformat()}.")
            if issue.get("description"):
                lines.append(str(issue["description"]))
            title = f"Linear {ident}: {_one_line(issue.get('title'))}" if ident else f"Linear: {issue.get('title')}"
            row = item(f"linear:{issue['id']}", "linear", title, "\n".join(x for x in lines if x),
                       when=due or (updated.isoformat(timespec="seconds") if updated else ""))
            key = (due is None, due or "", -(updated.timestamp() if updated else 0.0))
            picked.append((key, _linked(row, issue.get("url"), "linear")))
    picked.sort(key=lambda kv: kv[0])
    return [row for _key, row in picked[:LINEAR_MAX]]


# ---- the work Gmail and Calendar -------------------------------------------------------------------------------

def _labels(m: Mapping[str, Any]) -> list[str]:
    got = m.get("labelIds")
    return [str(x) for x in got] if isinstance(got, list) else []


def work_email_items(reader: Reader, now: datetime) -> list[dict[str, Any]]:
    """Blocking. Each other Gmail account's inbox of the last 7 days without promotions and social (updates'
    MAIL_QUERY), one item per thread with its newest message: ``wgmail:<account>/<thread>``. A thread the owner wrote
    in after that message (the account's sent mail of the same 7 days) says so."""
    out: list[dict[str, Any]] = []
    for alias, account in reader.accounts_for("work_email"):
        word = alias_word(alias)
        label = alias.strip().capitalize() or "Other"
        inbox = reader.call("GMAIL_FETCH_EMAILS", {"query": MAIL_QUERY, "max_results": WORK_MAIL_MAX,
                                                   "verbose": True, "include_payload": False}, account)
        sent = reader.call("GMAIL_FETCH_EMAILS", {"query": "in:sent newer_than:7d", "max_results": 50,
                                                  "verbose": False, "include_payload": False}, account)
        replied: dict[str, str] = {}
        for m in sent.get("messages") or []:
            if isinstance(m, Mapping) and isinstance(m.get("threadId"), str):
                when = _one_line(m.get("messageTimestamp"))
                replied[m["threadId"]] = max(replied.get(m["threadId"], ""), when)
        threads: dict[str, Mapping[str, Any]] = {}
        for m in inbox.get("messages") or []:
            if not isinstance(m, Mapping) or not isinstance(m.get("threadId"), str) or not _SAFE_ID.match(m["threadId"]):
                continue
            if "SENT" in _labels(m):
                continue
            old = threads.get(m["threadId"])
            if old is None or _one_line(m.get("messageTimestamp")) > _one_line(old.get("messageTimestamp")):
                threads[m["threadId"]] = m
        for thread, m in sorted(threads.items(), key=lambda kv: _one_line(kv[1].get("messageTimestamp")), reverse=True):
            preview = m.get("preview") if isinstance(m.get("preview"), Mapping) else {}
            text = str(m.get("messageText") or preview.get("body") or "")
            when = _one_line(m.get("messageTimestamp"))
            notes = []
            if replied.get(thread, "") > when:
                notes.append(f"The owner already replied after this message, on {replied[thread][:10]}.")
            body = "\n".join(notes + [text]) if notes else text
            row = item(f"wgmail:{word}/{thread}", "work_email", f"{label} email: {_one_line(m.get('subject')) or '(no subject)'}",
                       body, sender=m.get("sender"), when=_iso(when))
            out.append(_linked(row, m.get("display_url"), "work_email"))
    return out[:WORK_MAIL_MAX]


def work_calendar_items(reader: Reader, now: datetime) -> list[dict[str, Any]]:
    """Blocking. Each other calendar account's events of the next 7 days that the owner has not declined, soonest
    first, as updates.calendar_items reads the owner's own (an unanswered invitation is marked):
    ``wcal:<account>/<event>``."""
    out: list[dict[str, Any]] = []
    end = now + timedelta(days=WORK_CALENDAR_DAYS)
    for alias, account in reader.accounts_for("work_calendar"):
        word = alias_word(alias)
        label = alias.strip().capitalize() or "Other"
        data = reader.call("GOOGLECALENDAR_EVENTS_LIST", {
            "calendarId": "primary", "timeMin": now.isoformat(), "timeMax": end.isoformat(), "singleEvents": True,
            "orderBy": "startTime", "maxResults": 50}, account)
        seen: set[str] = set()
        for ev in data.get("items") or []:
            if not isinstance(ev, Mapping) or not isinstance(ev.get("id"), str) or not _SAFE_ID.match(ev["id"]) \
                    or ev["id"] in seen:
                continue
            row = event_item(ev, f"wcal:{word}/{ev['id']}", "work_calendar", f"{label} calendar: ")
            if row is not None:
                seen.add(ev["id"])
                out.append(_linked(row, ev.get("htmlLink"), "work_calendar"))
    return out


READERS: dict[str, Callable[..., list[dict[str, Any]]]] = {
    "slack": slack_items, "notion": notion_items, "linear": linear_items, "work_email": work_email_items,
    "work_calendar": work_calendar_items}


def reads(bridge: Any, now: datetime, limits: Mapping[str, float],
          mono: Callable[[], float] = time.monotonic) -> list[tuple[str, Optional[Callable[[], list[dict[str, Any]]]]]]:
    """``(kind, read)`` for each of KINDS, as updates.gather runs them; ``read`` is None (the source is off) without
    a bridge these sources can read through. One Reader serves them all, so the accounts are listed once."""
    if not usable(bridge):
        return [(kind, None) for kind in KINDS]
    reader = Reader(bridge)

    def make(kind: str) -> Callable[[], list[dict[str, Any]]]:
        if kind == "notion":
            return lambda: notion_items(reader, now, mono() + limits.get("notion", LIMITS["notion"]) * 0.7, mono)
        return lambda: READERS[kind](reader, now)

    return [(kind, make(kind)) for kind in KINDS]

