#!/usr/bin/env python3
"""Score which tool the chat brain picks for the owner's plain words: one label per case, production routing.

    export $(grep -E '^(OPENROUTER_API_KEY|TICKETMASTER_API_KEY)=' ~/.config/eve-bridge/env | xargs)
    cd bridge && uv run python tools/intent_eval.py CASES.json

CASES.json is a list (or {"cases": [...]}) of ``{"id", "text", "primary", "expect": [...], "group"?, "never"?}``:
the acceptable labels are ``primary`` and ``expect`` (both optional lists of labels; at least one is given).
``never`` lists labels that must not fire at any step of the case (NEVER_FIRED); a ``hard_negative`` case may not
accept a label that only acting tools reach (``acting_only_labels``): such a set is refused as a CONFLICT. The labels
are exactly LABELS: a capability per tool (``label``), ``ask`` and ``none`` for a reply with no tool.

**Scoring (bar v4, pre-registered 2026-10-09, fixed before any v4 result).** The brain's actions in the turn are
mapped to labels (``score``). A case passes when (a) its first action maps to an acceptable label, or (b) its first
action is a read-only lookup from PREP and its second action, made after the lookup returned a stub result
(``prep_stub``: short and neutral, nothing of the owner's is read), maps to one. PREP is exactly list_files, the
calendar's list/search events, Gmail's search/read threads, memory_search, watch_list, jobs_list and search_notes. A
reply with no tool is ``none`` when the case's primary is ``none``, whether or not it ends in a question; on every
other case it is ``ask`` when it ends in a question, else ``none``. A code path that answers before the model is the
tool it runs. A case whose group is ``hard_negative`` or whose primary is ``none`` or ``ask`` is a hard negative: an
acting tool fired at ANY step is listed apart (HARD_NEGATIVE_ACTED), whatever its expect says. NEW ships only if:
(1) overall >= 88%; (2) every primary with >= 6 cases scores >= BASE - 1 case; (3) HARD_NEGATIVE_ACTED is 0;
(4) events >= 80%, restaurant_booking >= 90%, tv_control >= 80%; (5) PROVIDER_ERRORS is 0.

**The turn is production's.** Each case is the first message of a fresh chat, as an Eve Link app turn (the app's
block rides the instructions, and Jev grades it as production does). First every path TelegramInlet._handle answers
before any model call is checked, in its order ("/spend", "claude on", a /rundown phrasing, the chief's words, then
the Fire TV, lights, Spotify and quick-answer matchers): such a message never reaches the brain, so it is scored by
the code path (``code_route``) as the tool it runs. Everything else goes through TelegramInlet._think itself, with
``_turn_said`` set as _answer_turn sets it, so the request is the one a typed message makes: the same instructions,
tools, turn note, and the PRODUCTION toolset router (Jev). **Nothing executes**: TelegramInlet._tool is replaced by
a recorder. A first call in PREP gets its stub back and the turn runs one more model round; any other call is
recorded and stops the turn, so an acting tool is never run. open_app is the one call let through (it only opens
toolsets), so the call it leads to is the one scored: the app the owner meant, not the first app it named.

**The production configuration** (the owner's Mac, read 2026-10-08; names and values of switches only, never a key):
EVE_TELEGRAM_MODEL=gpt-6-luna, EVE_JEV_ROUTE=openrouter (Jev over OpenRouter's decisions endpoint),
EVE_TOOLSET_ROUTER unset (auto: Jev on, toolset_router.SHIPPED), EVE_OPENTABLE=1, the chief and the grade
router at their defaults (auto: on). The harness sets each of those that the shell has not set (``PRODUCTION``), and
EVE_MENU_TOOLS=on unless the shell sets it (pass --menu-tools off to score the switch off). The run prints
the configuration it used and refuses to run on keywords alone unless --allow-keywords is given.

**What is lent**, with real schemas where eve defines them and nothing that can act: eve's own tools, web_search,
memory (its real tool schemas over an empty stub), the chat log (a temporary file), the second brain (its real tools
over an empty temporary vault), the watcher (a temporary list), Meet, lights, Spotify, Fire TV, america, OpenTable,
the chief (its real tools over an empty desk), the app maker, Share, the / menu (menu_tools.py, when this checkout
has it), events_search (when this checkout has it and TICKETMASTER_API_KEY is set), the direct apps (Gmail, Calendar
and Drive with google_rest's real tools; GitHub and Maps with a few canned tools, their MCP servers are remote) and
the Composio workspace (two canned meta tools). Every model call goes through OpenRouter (openrouter.py).

**Old checkouts.** The harness imports only what it finds: copied alone into an older checkout (BASE), it scores that
checkout's own tools and prompts. It needs nothing else from this tree.

Prints one PASS/FAIL line per case, totals by primary label, the overall percentage, the hard negatives that fired an
acting tool, Jev's errors and timeouts, PROVIDER_ERRORS (a model request that still fails after ATTEMPTS tries is a
FAIL and is counted there), and the spend.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import re
import sys
import tempfile
import time
from dataclasses import dataclass, field
from datetime import date, datetime, timedelta
from pathlib import Path
from types import SimpleNamespace
from typing import Any, Mapping, Optional, Sequence

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "src"))

ATTEMPTS = 3                       # a request that fails to arrive is sent again; the model's answer never is
NONE, ASK = "none", "ask"
UNKNOWN = "unknown_tool"              # a call to a name the turn did not offer: no case accepts it

LABELS = (
    "computer_task", "camera_photo", "screenshot", "send_file_to_owner", "mac_files_read", "room_notes",
    "memory_write", "coding_session", "deep_reasoning", "chat_history_recall", "memory_recall", "memory_forget",
    "web_search", "notes", "todos_read", "price_watch", "meet", "lights", "music", "tv_control", "camera_aim",
    "gov_legal_lookup",
    "restaurant_booking", "events", "jobs_and_reminders", "app_builder", "calendar_read", "calendar_write",
    "email_read", "email_send", "contacts_read", "maps_directions", "github", "slack", "notion", "linear",
    "claude_relay", "codex", "spend_report", "stealth", "stop_task", "phone_pairing", "ask", "none",
)

# The production switches (owner's Mac, 2026-10-08), set when the shell has not set them.
PRODUCTION = {"EVE_TELEGRAM_MODEL": "gpt-6-luna", "EVE_JEV_ROUTE": "openrouter", "EVE_OPENTABLE": "1"}

# tool name -> label. A tool whose label depends on its name's app or its arguments is in ``label``.
TOOL_LABELS: dict[str, str] = {
    "start_task": "computer_task", "steer_task": "computer_task", "take_photo": "camera_photo",
    "screenshot": "screenshot", "send_file": "send_file_to_owner", "list_files": "mac_files_read",
    "take_notes": "room_notes", "remember": "memory_write", "start_coding_session": "coding_session",
    "think_hard": "deep_reasoning", "recent_messages": "chat_history_recall", "memory_search": "memory_recall",
    "memory_read": "memory_recall", "forget_preview": "memory_forget", "forget_apply": "memory_forget",
    "web_search": "web_search", "list_todos": "todos_read", "america_ask": "gov_legal_lookup",
    "opentable_search": "restaurant_booking", "events_search": "events", "jobs_list": "jobs_and_reminders",
    "take_on": "jobs_and_reminders", "close_card": "jobs_and_reminders", "open_apps": "app_builder",
    "find_contact": "contacts_read", "claude_relay": "claude_relay", "codex_relay": "codex",
    "spend_report": "spend_report", "set_stealth": "stealth", "stop_task": "stop_task",
    "pair_eve_link": "phone_pairing", "read_file": "mac_files_read",
}
_PREFIXES = (("watch_", "price_watch"), ("meet_", "meet"), ("lights_", "lights"), ("spotify_", "music"),
             ("firetv_", "tv_control"), ("camera_", "camera_aim"))
_NOTES = ("capture_note", "search_notes", "read_note", "edit_note", "undo_note", "list_inbox", "file_note",
          "second_brain_workflow")
_MAKER = ("make_app", "change_app", "delete_app", "list_apps", "rename_app", "restore_app", "undo_app")
_APP_LABELS = {"gmail": "email_read", "calendar": "calendar_read", "maps": "maps_directions", "github": "github",
               "contacts": "contacts_read", "workspace": "slack"}
_SEND = re.compile(r"(send|reply|forward|draft)", re.I)

# Acting tools, for the hard-negative report: tool_gate.acting when this checkout has it, else this copy.
_ACTING = frozenset({"claude_relay", "set_stealth", "start_task", "steer_task", "start_coding_session", "screenshot",
                     "take_photo", "send_file", "lights_set", "lights_match", "spotify_control", "watch_add",
                     "watch_remove", "watch_pause", "watch_resume", "watch_set_end", "meet_join", "meet_leave",
                     "forget_apply", "make_app", "change_app", "delete_app", "rename_app", "restore_app", "undo_app",
                     "take_on", "close_card", "agentcard_buy", "agentcard_confirm", "opentable_search", "stop_task"})


# A direct app's tool word that changes something (a write), besides composio_tools.WRITE_VERBS: the calendar's
# quick add and RSVP, GitHub's push, fork and comment. A noun-too word counts only as the tool's first word.
_DIRECT_WRITE_WORDS = frozenset({"RESPOND", "RSVP", "QUICK", "PUSH", "FORK", "COMMENT", "REOPEN", "DISMISS", "REQUEST"})
_NOUN_TOO = frozenset({"LABEL", "DRAFT", "ORDER", "RUN", "SCHEDULE", "REQUEST", "COMMENT"})


def _direct_write(name: str) -> bool:
    """Whether a direct app's tool name (app__tool) reads as a write, by its words: calendar__quick_add_event,
    calendar__respond_to_event, drive__create_file, github__create_issue, github__merge_pull_request."""
    try:
        from eve_bridge.composio_tools import WRITE_VERBS
    except ImportError:
        WRITE_VERBS = frozenset({"SEND", "CREATE", "DELETE", "REMOVE", "UPDATE", "EDIT", "MERGE", "ADD", "MOVE",
                                 "UPLOAD", "INSERT", "WRITE", "PATCH", "SHARE", "TRASH", "REPLY", "FORWARD", "CLOSE"})
    words = [w for w in name.split("__", 1)[-1].upper().split("_") if w]
    verbs = WRITE_VERBS | _DIRECT_WRITE_WORDS
    return any(w in verbs and (w not in _NOUN_TOO or i == 0) for i, w in enumerate(words))


def _composio_writes(name: str, args: Mapping[str, Any]) -> bool:
    """Whether a Composio call changes something, by the slugs it executes (composio_tools.consequential_slugs: a
    multi-execute's writing slugs, a workbench script's, the remote code tools)."""
    try:
        from eve_bridge import composio_tools

        return bool(composio_tools.consequential_slugs(name, args))
    except ImportError:
        tools = args.get("tools") if isinstance(args.get("tools"), list) else []
        return name != "COMPOSIO_SEARCH_TOOLS" and (not tools or any(
            isinstance(t, Mapping) and re.search(r"_(SEND|CREATE|DELETE|UPDATE|POST|ADD|REPLY|INVITE)",
                                                 str(t.get("tool_slug") or "").upper()) for t in tools))


def acting(name: str, args: Mapping[str, Any], writes: Optional[set[str]] = None) -> bool:
    """Whether a first call changes something or opens a session: tool_gate's rule, plus opentable_search and
    stop_task (each starts or stops a Mac job). Writes in the owner's apps count too: a direct app's tool in
    ``writes`` (the tools its server does not mark read only) or named as a write (``_direct_write``), and a
    Composio call by the slugs it executes (``_composio_writes``)."""
    if not name:
        return False
    if name in ("opentable_search", "stop_task") or name in _ACTING:
        return True
    if "__" in name and ((writes is not None and name in writes) or _direct_write(name)):
        return True
    if name.startswith("COMPOSIO_") and _composio_writes(name, args):
        return True
    try:
        from eve_bridge import tool_gate

        if tool_gate.acting(name, args):
            return True
    except ImportError:
        if name == "pair_eve_link":
            return bool(args.get("new_link"))
        if name == "codex_relay":
            return str(args.get("action") or "") != "status"
        if name == "firetv_control":
            return str(args.get("action") or "") in ("power_on", "power_off", "type_text", "youtube_search")
    return label(name, args) in ("calendar_write", "email_send")


def _composio_label(args: Mapping[str, Any]) -> str:
    text = json.dumps(args).lower()
    for app in ("slack", "notion", "linear"):
        if app in text:
            return app
    if "gmail" in text:
        return "email_send" if _SEND.search(text) else "email_read"
    if "calendar" in text:
        return "calendar_write" if re.search(r"create|update|delete|insert|patch", text) else "calendar_read"
    return "slack"


_WORKSPACE_APPS = ("slack", "notion", "linear")


def open_app_label(args: Mapping[str, Any], text: str = "") -> str:
    """open_app only opens toolsets; the recorder lets it run and labels the call that follows (_ask). When none
    follows, the label is the app the owner meant: the one named app, or among several the one the owner's words
    name (the workspace is Slack, Notion or Linear, whichever the words name)."""
    apps = args.get("apps") if isinstance(args.get("apps"), list) else [args.get("app")]
    named = [str(a).lower() for a in apps if a]
    words = text.lower()
    pairs: list[tuple[str, str]] = []                      # (the word that names it, its label)
    for app in named:
        if app == "workspace":
            pairs += [(a, a) for a in _WORKSPACE_APPS if a in words] or [("workspace", "slack")]
        else:
            pairs.append((app, _APP_LABELS.get(app, app)))
    if not pairs:
        return "open_app"
    said = [lab for word, lab in pairs if word in words]
    return (said or [lab for _, lab in pairs])[0]


def label(name: str, args: Optional[Mapping[str, Any]] = None, *, writes: Optional[set[str]] = None,
          text: str = "") -> str:
    """The label a first tool call reaches. ``writes``: the direct apps' tool names that change something;
    ``text``: the owner's words (open_app's intent)."""
    args = args or {}
    if not name:
        return NONE
    if name in TOOL_LABELS:
        return TOOL_LABELS[name]
    for prefix, lab in _PREFIXES:
        if name.startswith(prefix):
            return lab
    if name in _NOTES:
        return "notes"
    if name in _MAKER:
        return "app_builder"
    if name == "open_app":
        return open_app_label(args, text)
    if "__" in name:
        app = name.split("__", 1)[0]
        write = name in writes if writes is not None else bool(re.search(r"create|update|delete|insert|send",
                                                                         name))
        if app == "calendar":
            return "calendar_write" if write else "calendar_read"
        if app == "gmail":
            return "email_send" if write or _SEND.search(name) else "email_read"
        return {"maps": "maps_directions", "github": "github", "contacts": "contacts_read"}.get(app, app)
    if name.startswith("COMPOSIO_"):
        return _composio_label(args)
    return name


def reply_label(text: str, primary: str = "") -> str:
    """A reply with no tool. On a case whose primary is ``none`` it is ``none``, whether or not it ends in a
    question (bar v4); on every other case it is ``ask`` when it ends by asking the owner something, else ``none``."""
    body = text.strip()
    if not body or primary == NONE:
        return NONE
    last = re.split(r"(?<=[.!?])\s+", body.splitlines()[-1].strip())[-1]
    return ASK if last.rstrip(" )\"'").endswith("?") else NONE


# ---- the v4 scorer (pre-registered 2026-10-09) ---------------------------------------------------------------

# The read-only lookups a turn may make before it acts: exactly these. A case whose first action is one of them is
# scored on its second action, made after the lookup returned a stub (PREP_STUB).
PREP = frozenset({"list_files", "calendar__list_events", "calendar__search_events", "gmail__search_threads",
                  "gmail__get_thread", "memory_search", "watch_list", "jobs_list", "search_notes"})


@dataclass(frozen=True)
class Step:
    """One action of the turn: a tool call (``tool``), or the reply that ended it (``tool`` ""), in model round
    ``round``; ``label`` is what it maps to, ``acted`` whether it changes something."""
    tool: str
    label: str
    acted: bool = False
    round: int = 1


def score(accept: Sequence[str], steps: Sequence[Step]) -> tuple[bool, str]:
    """(passed, the label credited). Passes when the first action maps to a label in ``accept``, or the first
    action is a PREP lookup and the second action, made in a later round (after the lookup's result came back),
    maps to one. Nothing else passes: no third action, no call written beside the lookup in its own round."""
    if not steps:
        return NONE in accept, NONE
    first = steps[0]
    if first.label in accept:
        return True, first.label
    if first.tool in PREP and first.label != UNKNOWN:
        second = steps[1] if len(steps) > 1 else None
        if second is None or second.round <= first.round:
            return False, first.label + ">" + (second.label if second is not None else "(stopped)")
        return second.label in accept, first.label + ">" + second.label
    return False, first.label


_STOP = frozenset("""a an the my me i im i'm u you your to for of on in at it its that this those these and or is are
was be can could would will pls please send find get give show tell open read whats what's what where which with from
off over here there just some any all up out into about do does did have has had need want wanna gonna thats that's
file files folder folders pdf doc docx txt jpg jpeg png csv xlsx spreadsheet mac laptop computer desktop downloads
documents pictures phone chat latest last new old called named anywhere drop attach shoot""".split())
_EXT = (("spreadsheet", "xlsx"), ("xlsx", "xlsx"), ("csv", "csv"), ("jpg", "jpg"), ("jpeg", "jpg"),
        ("png", "png"), ("photo", "jpg"), ("headshot", "jpg"), ("txt", "txt"), ("readme", "md"), ("docx", "docx"),
        ("doc ", "docx"), ("pdf", "pdf"))
_FOLDERS = ("desktop", "downloads", "documents", "pictures")


def _topic(text: str, limit: int = 3) -> list[str]:
    words = re.findall(r"[a-z0-9]+(?:'[a-z]+)?", text.lower())
    return [w for w in words if w not in _STOP and len(w) > 1][:limit]


def prep_stub(name: str, args: Mapping[str, Any], text: str) -> dict[str, Any]:
    """A short, neutral result for a PREP lookup: nothing of the owner's is read. A file listing holds one file
    named from the request's own words; a calendar or mail lookup one item titled from them; memory, the
    watches, the jobs and the notes come back empty."""
    topic = _topic(text)
    title = " ".join(topic) or "item"
    if name == "list_files":
        literal = re.search(r"[\w-]+\.[a-z0-9]{2,4}\b", text.lower())
        ext = next((e for word, e in _EXT if word in text.lower() + " "), "pdf")
        file = literal.group(0) if literal else "-".join(topic or ["file"]) + "." + ext
        where = str(args.get("path") or "") or "~/" + next((f.capitalize() for f in _FOLDERS if f in text.lower()),
                                                           "Documents")
        return {"ok": True, "folder": where, "files": [{"name": file, "bytes": 48213}], "more": 0}
    if name.startswith("calendar__"):
        # The event is at the time and on the day the request names ("move my 3pm", "tomorrow's 1:1"), else 2pm
        # today, or tomorrow when it says so (2026-10-09: a fixed 2pm made "move my 3pm to 4" find no 3pm, and asking was then right).
        at = re.search(r"\b(\d{1,2})(?::(\d{2}))?\s*(am|pm)\b", text.lower())
        hour = (int(at.group(1)) % 12 + (12 if at.group(3) == "pm" else 0)) if at else 14
        minute = int(at.group(2) or 0) if at else 0
        day = date.today() + timedelta(days=1 if re.search(r"\btomorrow'?s?\b", text.lower()) else 0)
        start = f"{day.isoformat()}T{hour:02d}:{minute:02d}:00-07:00"
        end = f"{day.isoformat()}T{(hour + 1) % 24:02d}:{minute:02d}:00-07:00"
        return {"timeZone": "America/Los_Angeles", "events": [
            {"eventId": "evt-1", "summary": title, "start": {"dateTime": start}, "end": {"dateTime": end},
             "calendar": "primary"}]}
    if name == "gmail__search_threads":
        return {"threads": [{"threadId": "thr-1", "subject": title, "from": "someone@example.com",
                             "date": "Thu, 8 Oct 2026 09:12:00 -0700", "messages": 1, "unread": False,
                             "snippet": ""}], "nextPageToken": None}
    if name == "gmail__get_thread":
        return {"threadId": str(args.get("threadId") or "thr-1"), "messages": [
            {"messageId": "msg-1", "from": "someone@example.com", "subject": title,
             "date": "Thu, 8 Oct 2026 09:12:00 -0700", "text": ""}]}
    if name == "memory_search":
        return {"ok": True, "results": []}
    if name == "watch_list":
        return {"ok": True, "watches": []}
    if name == "jobs_list":
        return {"ok": True, "jobs": []}
    if name == "search_notes":
        return {"ok": True, "notes": []}
    return {"ok": True}


# A quick answer (quick_answers.py) by kind: the live facts are what web_search would have found; the rest is a
# plain reply written by code.
_QUICK_LABELS = {"weather": "web_search", "sun": "web_search", "air": "web_search"}


def code_route(text: str, inlet: Any = None) -> str:
    """The label TelegramInlet._handle answers by code before any model call, or "" when the words reach the
    brain: the same checks in the same order, read from this checkout's telegram.py (each only if it has it). With
    ``inlet`` (the harness's own, as _brain builds it) the inlet's own matchers run too, as _handle runs them: the
    chief's words while the chief is lent, then the Fire TV, the lights, Spotify and the quick answers. A fresh
    chat has no open "new claude" tree and no question waiting, so those two paths never take a case here; each
    case's chat is fresh (_ask)."""
    from eve_bridge import claude_launch, rundown
    from eve_bridge import telegram as tg

    stripped = text.strip()
    word = text.lower().rstrip(".! ")
    opentable_cmd = getattr(getattr(tg, "opentable", None), "COMMAND", None)
    if (opentable_cmd is not None and opentable_cmd.fullmatch(stripped)) or re.match(r"^/opentable\b", stripped,
                                                                                    re.I):
        return "restaurant_booking"
    america = getattr(tg, "AMERICA_COMMAND", None)
    if america is not None and america.fullmatch(stripped):
        return "gov_legal_lookup"
    meet = getattr(tg, "MEET_COMMAND", None)
    if meet is not None and meet.match(stripped):
        return "meet"
    watch_cmd = getattr(tg, "WATCH_COMMAND", None)
    if watch_cmd is not None and watch_cmd.match(stripped):
        return ""                                  # rewritten "Watch …" and sent to the brain (brain_text)
    if claude_launch.TRIGGER.match(stripped):
        return "coding_session"
    if rundown.matches(text):
        return "daily_rundown"
    if re.fullmatch(r"/?codex(?:\s+([^:].*))?", stripped, re.I):
        return "codex"
    if word in getattr(tg, "STOP_WORDS", ()):
        return "stop_task"
    if word in getattr(tg, "CLAUDE_OFF", ()) or word in getattr(tg, "CLAUDE_ON", ()) or re.fullmatch(
            r"/?claude[ _]on\s+(.+)", word):
        return "claude_relay"
    if re.match(r"^/?codex\s*:\s*(.+)$", text, re.I | re.S):
        return "codex"                             # typed straight to Codex
    claude_prefix = getattr(tg, "CLAUDE_PREFIX", None)
    if claude_prefix is not None and claude_prefix.match(text):
        return "claude_relay"                      # typed straight into the Claude session
    for attr, lab in (("APPS_WORDS", "app_builder"), ("SPEND_WORDS", "spend_report"), ("STICK_WORDS", "phone_pairing"),
                      ("STICK_NEW_WORDS", "phone_pairing")):
        if word in getattr(tg, attr, ()):
            return lab
    if inlet is not None and getattr(inlet, "_chief_word", None) is not None and inlet._chief_word(text) is not None:
        return "jobs_and_reminders"
    if inlet is None and re.fullmatch(r"/jobs(?:@\w+)?", stripped, re.I):
        return "jobs_and_reminders"
    for attr, lab in (("WATCH_WORDS", "price_watch"), ("STEALTH_ON", "stealth"), ("STEALTH_OFF", "stealth")):
        if word in getattr(tg, attr, ()):
            return lab
    screen = getattr(tg, "SCREEN_NOW", None)
    if screen is not None and screen.match(text):
        return "screenshot"
    if word == "/start":
        return NONE
    if inlet is None:
        return ""
    from eve_bridge import firetv, obsbot_panel, spotify

    if getattr(inlet, "_firetv", None) is not None and firetv.match(text) is not None:
        return "tv_control"
    if getattr(inlet, "_obsbot", None) is not None and obsbot_panel.match(text) is not None:
        return "camera_aim"
    matcher = getattr(inlet, "_eval_lights", None)
    if matcher is not None and matcher.match(text) is not None:
        return "lights"
    if getattr(inlet, "_spotify", None) is not None and spotify.match(text) is not None:
        return "music"
    quick = getattr(inlet, "_quick", None)
    ask = quick.match(text) if quick is not None else None
    if ask is not None:
        return _QUICK_LABELS.get(str(getattr(ask, "kind", "")), NONE)
    return ""


def brain_text(text: str) -> str:
    """The words the brain is given: "/watch <words>" reaches it as "Watch <words>" (_handle), the rest as typed."""
    from eve_bridge import telegram as tg

    watch_cmd = getattr(tg, "WATCH_COMMAND", None)
    m = watch_cmd.match(text.strip()) if watch_cmd is not None else None
    return "Watch " + m.group(1).strip() if m else text


def acceptable(case: Mapping[str, Any]) -> tuple[str, ...]:
    out: list[str] = []
    for key in ("primary", "expect"):
        raw = case.get(key)
        out += [str(x) for x in raw] if isinstance(raw, list) else [str(raw)] if raw else []
    return tuple(dict.fromkeys(out))


def primary(case: Mapping[str, Any]) -> str:
    raw = case.get("primary")
    if isinstance(raw, list):
        return str(raw[0]) if raw else ""
    return str(raw or (acceptable(case) or ("",))[0])


def load_cases(path: Path) -> list[dict[str, Any]]:
    raw = json.loads(path.read_text(encoding="utf-8"))
    cases = raw.get("cases") if isinstance(raw, dict) else raw
    if not isinstance(cases, list) or not all(isinstance(c, dict) and "text" in c and acceptable(c) for c in cases):
        raise SystemExit(f"BAD_CASES: {path.name} is not a list of {{id, text, primary, expect}}")
    unknown = sorted({lab for c in cases for lab in acceptable(c)} - set(LABELS))
    if unknown:
        print(f"   note: labels outside LABELS in the cases: {', '.join(unknown)}")
    clash = [str(c.get("id", "?")) for c in cases if str(c.get("group") or "") == "hard_negative"
             and set(acceptable(c)) & acting_only_labels()]
    if clash:
        # a hard negative fails on any acting step, so it cannot also accept a label only acting tools reach
        # (2026-10-11: two blind camera cases accepted computer_task and failed by construction). Use "never"
        # for the labels that must not fire, and leave such a case out of hard_negative.
        raise SystemExit(f"CONFLICT: hard_negative cases accept an acting-only label: {', '.join(clash)}")
    bad_never = sorted({lab for c in cases for lab in c.get("never", [])} - set(LABELS))
    if bad_never:
        raise SystemExit(f"BAD_CASES: never names labels outside LABELS: {', '.join(bad_never)}")
    return cases


def acting_only_labels() -> frozenset[str]:
    """Labels that only acting tools reach (every TOOL_LABELS tool with that label acts). A label that a read can
    also reach (jobs_and_reminders, camera_aim's camera_status, the prefixed families) is not one."""
    by: dict[str, list[bool]] = {}
    for name, lab in TOOL_LABELS.items():
        by.setdefault(lab, []).append(acting(name, {}))
    prefixed = {lab for _, lab in _PREFIXES}
    return frozenset(lab for lab, acts in by.items() if all(acts) and lab not in prefixed)


@dataclass(frozen=True)
class Row:
    id: str
    primary: str
    accept: tuple[str, ...]
    steps: tuple[Step, ...]        # the turn's actions in order (score); a code path is one step
    via: str                       # "code" | "brain"
    group: str = ""                # the case's group ("hard_negative", ...), when it has one
    error: str = ""                # the provider error's type, or ""
    jev: str = ""                  # Jev's error or timeout on this case, or ""
    extra: dict[str, Any] = field(default_factory=dict, compare=False)
    never: tuple[str, ...] = ()    # labels that must not fire at any step ("never" in the case), whatever passes

    @property
    def never_fired(self) -> list[str]:
        return [s.label for s in self.steps if s.label in self.never]

    @property
    def scored(self) -> tuple[bool, str]:
        return score(self.accept, self.steps)

    @property
    def passed(self) -> bool:
        return not self.error and self.scored[0]

    @property
    def got(self) -> str:
        return self.scored[1]

    @property
    def tool(self) -> str:
        """The tools called, in order ("" for a reply or a code word)."""
        return ">".join(s.tool for s in self.steps if s.tool)

    @property
    def acted(self) -> bool:
        """An acting tool fired at any step."""
        return any(s.acted for s in self.steps)

    @property
    def hard_negative(self) -> bool:
        return self.primary in (NONE, ASK) or self.group == "hard_negative"


def totals(rows: Sequence[Row]) -> dict[str, tuple[int, int]]:
    """primary label -> (passed, cases)."""
    out: dict[str, tuple[int, int]] = {}
    for r in rows:
        p, n = out.get(r.primary, (0, 0))
        out[r.primary] = (p + int(r.passed), n + 1)
    return out


def report(rows: Sequence[Row]) -> None:
    for r in rows:
        mark = "PASS" if r.passed else "FAIL"
        what = f"ERROR {r.error}" if r.error else f"got={r.got}" + (f" ({r.tool})" if r.tool else "") + (
            " [code word]" if r.via == "code" else "")
        print(f"{mark} {r.id}: primary={r.primary} accept={'|'.join(r.accept)} {what}")
    print("-- by primary label --")
    for key, (p, n) in sorted(totals(rows).items()):
        print(f"   {key}: {p}/{n}")
    passed = sum(1 for r in rows if r.passed)
    pct = 100.0 * passed / len(rows) if rows else 0.0
    print(f"TOTAL {passed}/{len(rows)} ({pct:.1f}%)")
    banned = [r for r in rows if r.never_fired]
    print(f"NEVER_FIRED {len(banned)}" + (": " + ", ".join(f"{r.id}->{'>'.join(r.never_fired)}" for r in banned)
                                          if banned else ""))
    fired = [r for r in rows if r.hard_negative and r.acted]
    print(f"HARD_NEGATIVE_ACTED {len(fired)}" + (": " + ", ".join(
        f"{r.id}->{'>'.join(s.tool or s.label for s in r.steps if s.acted)}" for r in fired) if fired else ""))
    jev = [r for r in rows if r.jev]
    print(f"JEV_ERRORS {len(jev)}" + (": " + ", ".join(f"{r.id} {r.jev}" for r in jev) if jev else ""))
    errors = [r for r in rows if r.error]
    print(f"PROVIDER_ERRORS {len(errors)}" + (": " + ", ".join(f"{r.id} {r.error}" for r in errors) if errors else ""))


# ---- the lent services: real schemas, nothing that can act --------------------------------------------------

class _Stopped(Exception):
    """The recorder stopped the turn at its first tool call: nothing runs."""


class _NoApi:
    """The Telegram bot API, lent to nothing: every call is a bug in the eval."""

    def __getattr__(self, name: str) -> Any:
        raise RuntimeError(f"the intent eval never talks to Telegram ({name})")


class _Lent:
    """A service as the brain sees it: its tools and its block, from the module's own constants."""

    def __init__(self, tools: Any, block: str) -> None:
        self._tools, self._block = tools, block

    def tools(self) -> list[dict[str, Any]]:
        return json.loads(json.dumps(self._tools))

    def instructions(self, *_a: Any, **_k: Any) -> str:
        return self._block

    def kinds(self) -> list[str]:
        return []


class _Memory:
    """memory.Memory as a turn reads it: the real tool schemas, an empty profile and day."""
    retry = False
    hybrid = False

    def tools(self, *, voice: bool = False) -> list[dict[str, Any]]:
        from eve_bridge import memory

        return json.loads(json.dumps(list(memory.TOOLS)))

    def profile(self) -> str:
        return ""

    def profile_for_voice(self) -> str:
        return ""

    def today(self, *_a: Any, **_k: Any) -> str:
        return ""

    def __getattr__(self, name: str) -> Any:
        return lambda *a, **k: None


class _Tunnel:
    url = "https://miniapp.invalid"


def _canned(name: str, desc: str, reads: bool, props: Optional[dict[str, Any]] = None) -> dict[str, Any]:
    return {"name": name, "description": desc, "annotations": {"readOnlyHint": reads},
            "inputSchema": {"type": "object", "properties": props or {"query": {"type": "string"}}}}


# GitHub's and Maps' MCP servers are remote: a few tools in their shape, so a case that needs them can reach them.
_REMOTE_TOOLS = {
    "github": [_canned("search_repositories", "Search the owner's GitHub repositories.", True),
               _canned("list_pull_requests", "List pull requests in a repository.", True),
               _canned("list_issues", "List issues in a repository.", True),
               _canned("actions_list", "List workflow runs (CI status) of a repository.", True),
               _canned("actions_get", "Get one workflow run or job of a repository.", True),
               _canned("get_job_logs", "The logs of a workflow job, to see what failed.", True),
               # the rest of the real server's everyday reads (2026-10-09: with no list_commits here, "the latest
               # commits on my website repo" had no GitHub tool and went to a computer task or Composio, which
               # production, with the real server's 47 tools, never sees)
               _canned("list_commits", "List commits of a repository's branch.", True),
               _canned("get_file_contents", "Read a file or a folder of a repository.", True),
               _canned("search_issues", "Search issues across the owner's repositories.", True),
               _canned("search_pull_requests", "Search pull requests across the owner's repositories.", True),
               _canned("list_branches", "List a repository's branches.", True),
               _canned("get_me", "The signed-in GitHub user: their login and profile.", True),
               _canned("star_repository", "Star a repository.", False),
               _canned("create_issue", "Open an issue in a repository.", False)],
    "maps": [_canned("search_places", "Find places by text: addresses, businesses, places nearby.", True),
             _canned("compute_routes", "Directions and travel time between places.", True),
             _canned("lookup_weather", "The weather and forecast for a place.", True)],
}


def _direct_apps() -> Any:
    from eve_bridge import apps_mcp

    class _Remote:
        def __init__(self, app: str) -> None:
            self.app = app

        def list_tools(self) -> list[dict[str, Any]]:
            return [dict(t) for t in _REMOTE_TOOLS[self.app]]

    tok = SimpleNamespace(available=True)
    bridge = apps_mcp.AppsBridge(apps_mcp.AppsConfig(enabled=True),
                                 client_factory=lambda server, token: _Remote(server.app),
                                 tokens={"google": tok, "github": tok, "maps": tok})
    bridge.start()
    return bridge


class _Workspace:
    """The Composio session as the brain sees it: two meta tools, named as Composio names them."""
    started = True

    def __init__(self) -> None:
        self._tools = [
            {"type": "function", "name": "COMPOSIO_SEARCH_TOOLS", "strict": False,
             "description": "Find the tool for a job in the owner's Slack, Notion or Linear.",
             "parameters": {"type": "object", "properties": {"queries": {"type": "array", "items": {
                 "type": "object", "properties": {"use_case": {"type": "string"}}}}}}},
            {"type": "function", "name": "COMPOSIO_MULTI_EXECUTE_TOOL", "strict": False,
             "description": "Run Slack, Notion or Linear tools by slug (SLACK_..., NOTION_..., LINEAR_...).",
             "parameters": {"type": "object", "properties": {"tools": {"type": "array", "items": {
                 "type": "object", "properties": {"tool_slug": {"type": "string"},
                                                  "arguments": {"type": "object"}}}}}}},
        ]
        self.names = frozenset(t["name"] for t in self._tools)

    def tools(self) -> list[dict[str, Any]]:
        return json.loads(json.dumps(self._tools))

    def accounts_note(self) -> str:
        return ""


def _environment(scratch: Path, model: str, menu: str) -> list[str]:
    """The production switches the shell has not set, the door's placeholders, and every file the turn writes in
    ``scratch``. Keys are only the ones already exported; nothing is read from the owner's env file."""
    notes = []
    for key, value in PRODUCTION.items():
        if not os.environ.get(key):
            os.environ[key] = value
    if model:
        os.environ["EVE_TELEGRAM_MODEL"] = model
    if menu:
        os.environ["EVE_MENU_TOOLS"] = menu
    elif not os.environ.get("EVE_MENU_TOOLS"):
        os.environ["EVE_MENU_TOOLS"] = "on"
    os.environ.update({
        "EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": "0:eval", "EVE_TELEGRAM_OWNER": "1",
        "EVE_SPEND_DIR": str(scratch / "spend"), "EVE_WATCH_FILE": str(scratch / "watches.json"),
        "EVE_CHAT_LOG_FILE": str(scratch / "chat" / "log.json"),
    })
    for key in ("EVE_TELEGRAM_MODEL", "EVE_JEV_ROUTE", "EVE_TOOLSET_ROUTER", "EVE_OPENTABLE",
                "EVE_CHIEF", "EVE_GRADE_ROUTER", "EVE_MENU_TOOLS"):
        notes.append(f"{key}={os.environ.get(key, '') or '(unset: default)'}")
    return notes


def _brain(scratch: Path, allow_keywords: bool) -> tuple[Any, list[str], Any]:
    from eve_bridge import (
        america,
        apps_maker,
        chat_log,
        chief,
        firetv,
        lights,
        meet,
        obsbot_panel,
        openrouter,
        share,
        spotify,
        toolset_router,
        watch,
    )
    from eve_bridge import telegram as tg
    from eve_bridge.computer_agent import make_response_creator

    if not openrouter.available():
        raise SystemExit("NO_KEY: export OPENROUTER_API_KEY for the model calls")
    cfg = tg.configured()
    real = make_response_creator()

    async def create(payload: dict[str, Any]) -> dict[str, Any]:
        last: Optional[BaseException] = None
        for attempt in range(ATTEMPTS):
            try:
                return await real(payload)
            except Exception as e:  # noqa: BLE001 — the type only; tried again, then the case is an error
                last = e
                await asyncio.sleep(2.0 * (attempt + 1))
        assert last is not None
        raise last

    (scratch / "vault").mkdir(parents=True, exist_ok=True)
    (scratch / "chat").mkdir(parents=True, exist_ok=True)
    direct = _direct_apps()
    lights_block = lights.INSTRUCTIONS_HEAD + "\nThe lights:\n- desk lamp (room: office)\n- ceiling (room: living room)"
    lent: dict[str, Any] = dict(
        watcher=watch.make_watcher(), meeter=_Lent(meet.TOOLS, meet.INSTRUCTIONS_BLOCK),
        lights=_Lent(lights.TOOLS, lights_block), spotify=_Lent(spotify.TOOLS, spotify.INSTRUCTIONS),
        firetv=_Lent(firetv.TOOLS, firetv.INSTRUCTIONS), america=_Lent([america.TOOL], america.INSTRUCTIONS),
        obsbot=_Lent(obsbot_panel.TOOLS, obsbot_panel.INSTRUCTIONS),
        share=_Lent(share.TOOLS, share.INSTRUCTIONS), memory=_Memory(), vault=SimpleNamespace(root=scratch / "vault"),
        chat_log=chat_log.ChatLog(scratch / "chat" / "log.json"), direct=direct, apps=_Workspace(),
        opentable_enabled=True, agent_factory=lambda *a, **k: None, brief=lambda: "")
    inlet = tg.TelegramInlet(cfg, _NoApi(), create, **lent)
    if chief.enabled():
        inlet.chief = SimpleNamespace(live=chief.LIVE, for_turn=lambda: "")
    inlet._maker = apps_maker.ChatMaker(_Tunnel(), lambda coro, name: coro.close())
    # the lights' code path (_handle: "lights off", "lamp blue"), over the same two lights the brain is shown; it
    # only matches, it never reaches a light
    inlet._eval_lights = lights.Lights([lights.Light(name="desk lamp", kind="govee", room="office"),
                                        lights.Light(name="ceiling", kind="govee", room="living room")])
    jev = getattr(inlet, "_toolset_route", None) is not None
    if not jev and not allow_keywords:
        raise SystemExit("NO_JEV: the toolset router is not on Jev (production is). Export OPENROUTER_API_KEY with "
                         "EVE_JEV_ROUTE=openrouter, or pass --allow-keywords to score keywords only.")
    have = {"menu": _has("menu_tools"), "events": _has("events") and bool(os.environ.get("TICKETMASTER_API_KEY")),
            "taint gate": _has("tool_gate")}
    notes = [f"model {cfg.model}, effort {cfg.effort}",
             f"toolset router: {toolset_router.mode()} ({'Jev' if jev else 'keywords only'}), "
             f"chief {'on' if inlet.chief is not None else 'off'}",
             "this checkout has: " + ", ".join(f"{k} {'yes' if v else 'no'}" for k, v in have.items()),
             "lent: own tools, web_search, memory, chat log, second brain, watch, meet, lights, spotify, fire tv, "
             "obsbot camera, "
             "america, share, opentable, chief, maker, direct apps (" + ", ".join(direct.apps) + "), workspace"]
    writes = {n for n in direct.names if "__" in n and not direct.reads(n)}
    return inlet, notes, writes


def _has(module: str) -> bool:
    try:
        __import__(f"eve_bridge.{module}")
        return True
    except ImportError:
        return False


_JEV_SEEN: list[str] = []


def _watch_jev() -> None:
    """Jev's error or timeout on each case, as route_turn reports it (it never raises into the turn)."""
    from eve_bridge import toolset_router

    real = toolset_router.route_turn

    def route_turn(*a: Any, **k: Any) -> Any:
        out = real(*a, **k)
        err = getattr(out, "error", "") or ("timeout" if getattr(out, "timed_out", False) else "")
        if err:
            _JEV_SEEN.append(str(err))
        return out

    toolset_router.route_turn = route_turn


# A code path's label that changes something when it runs (lights, the TV, a session, a booking's browser task).
# Counted as acting whatever the words asked for, so a hard negative a matcher takes is never missed.
CODE_ACTING = frozenset({"tv_control", "camera_aim", "lights", "music", "stealth", "claude_relay", "codex", "coding_session",
                         "stop_task", "restaurant_booking", "meet", "screenshot"})


async def _ask(inlet: Any, case: Mapping[str, Any], i: int, writes: set[str]) -> Row:
    from eve_bridge import telegram as tg

    text = str(case["text"])
    ident, want, first = str(case.get("id") or i), acceptable(case), primary(case)
    group = str(case.get("group") or "")
    never = tuple(str(x) for x in case.get("never") or ())
    by_code = code_route(text, inlet)
    if by_code:
        return Row(ident, first, want, (Step("", by_code, by_code in CODE_ACTING),), "code", group, never=never)
    text = brain_text(text)
    steps: list[Step] = []
    opened: list[dict[str, Any]] = []
    rounds = [0]
    real_tool = type(inlet)._tool
    real_create = inlet._create

    async def counted(payload: dict[str, Any]) -> dict[str, Any]:
        rounds[0] += 1
        return await real_create(payload)

    async def record(name: str, args: dict[str, Any], chat_id: int) -> dict[str, Any]:
        if name == "open_app" and not opened:
            # open_app only opens toolsets (no side effect): it runs, and the call it leads to is the one scored,
            # as production's next round makes it
            opened.append(args)
            return await real_tool(inlet, name, args, chat_id)
        steps.append(Step(name, label(name, args, writes=writes, text=text), acting(name, args, writes), rounds[0]))
        if len(steps) == 1 and name in PREP:
            return prep_stub(name, args, text)     # a lookup first: a stub comes back and one more round runs
        raise _Stopped                             # nothing acting ever runs

    real_parse = tg.parse_response

    def parse(response: Any, allowed: Any = (), **kw: Any) -> Any:
        # A call to a name the turn did not offer is refused in _think without reaching _tool, and the next round
        # may repair it. It is an action all the same: recorded as UNKNOWN (no label accepts it), and the turn stops.
        calls, said, carry = real_parse(response, allowed, **kw)
        bad = next((k for k, c in enumerate(calls) if c.get("unknown")), None)
        if bad is not None:
            for c in calls[:bad]:
                steps.append(Step(c["name"], label(c["name"], c["args"], writes=writes, text=text),
                                  acting(c["name"], c["args"], writes), rounds[0]))
            steps.append(Step(calls[bad]["name"] or "?", UNKNOWN, False, rounds[0]))
            raise _Stopped
        return calls, said, carry

    inlet._tool = record
    inlet._create = counted
    tg.parse_response = parse

    async def no_owner_to_ask(name: str, args: Any, chat_id: int) -> None:
        return None                   # the taint gate passes the call on to the recorder, which runs nothing

    inlet._gate = no_owner_to_ask
    inlet._turn_said = text                      # as _answer_turn sets it: the toolset router reads it
    inlet._turn_request = 0
    del _JEV_SEEN[:]
    try:
        from eve_bridge import tool_gate

        tool_gate.MARKS.clear()                  # each case is a fresh chat
    except ImportError:
        pass
    mark = tg._FROM_APP.set(f"eval-{ident}")      # an app turn: link_chat's block rides the request
    reply: Optional[str] = None
    try:
        reply, _rounds = await inlet._think([tg.message_item("user", text)], 0, daily=False, tail=0)
    except _Stopped:
        pass
    except Exception as e:  # noqa: BLE001 — the type only
        return Row(ident, first, want, tuple(steps), "brain", group, type(e).__name__, jev=",".join(_JEV_SEEN),
                   never=never)
    finally:
        tg._FROM_APP.reset(mark)
        inlet._turn_said = ""
        inlet._create = real_create
        tg.parse_response = real_parse
    jev = ",".join(_JEV_SEEN)
    if reply is not None:
        if not steps and opened:
            # open_app, then a reply: the app the owner meant (open_app_label)
            steps.append(Step("open_app", open_app_label(opened[0], text), False, rounds[0]))
        else:
            steps.append(Step("", reply_label(reply or "", first), False, rounds[0]))
    return Row(ident, first, want, tuple(steps), "brain", group, jev=jev, never=never)


def run(path: Path, model: str = "", menu: str = "", allow_keywords: bool = False,
        require: Sequence[str] = ()) -> int:
    cases = load_cases(path)
    with tempfile.TemporaryDirectory(prefix="intent-eval-") as tmp:
        scratch = Path(tmp)
        env_notes = _environment(scratch, model, menu)
        from eve_bridge import spend

        spend.set_dir(scratch / "spend")
        _watch_jev()
        inlet, notes, writes = _brain(scratch, allow_keywords)
        print("   env: " + " ".join(env_notes))
        for n in notes:
            print("   " + n)
        print(f"   cases: {path.name}, {len(cases)}")

        async def go() -> list[Row]:
            rows = []
            t0 = time.monotonic()
            for i, case in enumerate(cases):
                rows.append(await _ask(inlet, case, i, writes))
            print(f"   {len(rows)} cases in {time.monotonic() - t0:.0f} s")
            return rows

        rows = asyncio.run(go())
        cost = spend.totals(spend.day_rows(date.today().isoformat()))
    report(rows)
    print(f"   cost: ${cost['usd']:.4f} over {cost['calls']} model calls ({cost['unpriced']} unpriced); "
          f"scored {datetime.now().isoformat(timespec='seconds')}")
    if not require:
        return 0
    missed = bars(rows, require)
    for m in missed:
        print("BAR MISSED: " + m)
    if missed:
        return 1
    print("INTENT_BAR_OK")
    return 0


def bars(rows: Sequence[Row], require: Sequence[str]) -> list[str]:
    """The --require bars a run missed: a label's pass rate under its PCT, a "never" label that fired, a hard
    negative that acted, or a provider error. A label with no cases misses (a bar over nothing proves nothing)."""
    missed = []
    by = totals(rows)
    for spec in require:
        lab, _, pct = spec.partition("=")
        if lab not in LABELS or not pct:
            missed.append(f"{spec}: not LABEL=PCT with a known label")
            continue
        p, n = by.get(lab, (0, 0))
        if n == 0 or 100.0 * p / n < float(pct):
            missed.append(f"{lab} {p}/{n} under {pct}%")
    banned = [r.id for r in rows if r.never_fired]
    if banned:
        missed.append("NEVER_FIRED " + ", ".join(banned))
    acted = [r.id for r in rows if r.hard_negative and r.acted]
    if acted:
        missed.append("HARD_NEGATIVE_ACTED " + ", ".join(acted))
    errors = [r.id for r in rows if r.error]
    if errors:
        missed.append("PROVIDER_ERRORS " + ", ".join(errors))
    return missed


def main(argv: Optional[list[str]] = None) -> int:
    p = argparse.ArgumentParser(prog="intent_eval", description=__doc__.split("\n\n")[0])
    p.add_argument("cases", help="a JSON list of {id, text, primary, expect}")
    p.add_argument("--model", default="", help="the chat model (default: the production one, gpt-6-luna)")
    p.add_argument("--menu-tools", default="", choices=("", "on", "off"),
                   help="EVE_MENU_TOOLS for the run (default: the shell's, else on)")
    p.add_argument("--allow-keywords", action="store_true", help="run even when Jev is not reachable")
    p.add_argument("--require", action="append", default=[], metavar="LABEL=PCT",
                   help="a bar for this run: the cases whose primary is LABEL score at least PCT%%; with any --require, "
                        "HARD_NEGATIVE_ACTED and PROVIDER_ERRORS must also be 0. Prints INTENT_BAR_OK when all hold")
    args = p.parse_args(argv)
    return run(Path(args.cases), args.model, args.menu_tools, args.allow_keywords, args.require)


if __name__ == "__main__":
    sys.exit(main())
