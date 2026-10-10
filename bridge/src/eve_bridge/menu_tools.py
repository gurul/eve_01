"""The / menu's commands as the chat brain's tools: the same capability from plain words.

Owner, 2026-10-08: "expose all the menu items natively as well, the agent should be smart enough to understand what
it has access to and make correct decision on what tool/api call to use". The / menu (telegram.BOT_COMMANDS) works
when its word is typed, and code answers it before any model call. Anything worded otherwise ("what did you spend
today", "hook me into claude", "go quiet for a bit") reached the brain, which had no tool for it and could only
guess. These tools close that gap. Each reaches the SAME handler the command uses (telegram.py, ``_tool_menu``):
nothing here re-implements a command, and each keeps exactly the confirmations its command has (the Claude
relay's session picker, Codex's folder picker; the code words themselves still win before the brain).

The commands that already had a tool are not repeated: /new_claude is start_coding_session, /screenshot is
screenshot, /stop is stop_task, /watch is the watch tools, /meet the meet tools, /jobs the chief's, /america and
/opentable their own.

Which tools a turn carries (toolset_router.ALWAYS): these are small and fixed, so every turn carries them, outside
the routed toolsets; the routed toolsets and their fitted gates are unchanged.

Two kinds of result:

* a hand-off (HANDS_OFF): the command's own handler sent its message (a button, a picker, the relay's line), so
  the result says ``end_turn`` and the turn ends there with no second model call, as start_task does. daily_rundown
  is one: it hands the owner's message to a rundown turn of its own (telegram._tool_menu), which offers only the
  rundown's read tools, exactly as the typed /rundown does (review, 2026-10-08: reading mail in a turn that kept
  every acting tool let an email's words reach them);
* a reading (spend_report): the result is data for the brain to answer from.

The switch (``EVE_MENU_TOOLS``, 2026-10-08): on, every turn carries these tools, events_search while
Ticketmaster's key is set, and the tool-choice lines (telegram.intent_block); off, nothing of it is in any request,
byte for byte as before. SHIPPED is the default and is set by the merge step, after the blind intent eval passed.
Whatever the switch says, the taint gate (tool_gate.py) guards every acting tool: in the chat
(telegram.TelegramInlet._gate, a firm yes/no) and out loud (voice_agent.VoiceSession._gate, a spoken yes).
"""

from __future__ import annotations

import os
from typing import Any, Mapping, Optional

# True since 2026-10-09: the v4 blind set (318 cases, scored once per arm) passed its pre-registered bar, NEW 95.0%
# against live 72.6% (bar 88%), events 12/12, restaurant_booking 12/12, tv_control 12/12, zero acting fires on hard
# negatives, zero provider errors (docs/toolset-router.md). EVE_MENU_TOOLS=off turns it off.
SHIPPED = True
MODES = ("on", "off")


def enabled(environ: Optional[Mapping[str, str]] = None) -> bool:
    """EVE_MENU_TOOLS = on | off (1/true/yes, 0/false/no too); unset or anything else is SHIPPED."""
    env = os.environ if environ is None else environ
    raw = (env.get("EVE_MENU_TOOLS") or "").strip().lower()
    if raw in ("1", "true", "yes", "on"):
        return True
    if raw in ("0", "false", "no", "off"):
        return False
    return SHIPPED

SPEND, RUNDOWN, CLAUDE, CODEX, STEALTH, APPS, LINK = (
    "spend_report", "daily_rundown", "claude_relay", "codex_relay", "set_stealth", "open_apps", "pair_eve_link")

_NO_ARGS: dict[str, Any] = {"type": "object", "additionalProperties": False, "required": [], "properties": {}}

TOOLS: list[dict[str, Any]] = [
    {
        "type": "function", "name": SPEND, "strict": True,
        "description": ("What eve has spent on models and services: today, yesterday, this month, the features "
                        "that cost most, and OpenRouter's own figure. Use it when the owner asks what eve cost or "
                        "spent, what their bill for eve is, or what a task cost; answer from it."),
        "parameters": _NO_ARGS,
    },
    {
        "type": "function", "name": RUNDOWN, "strict": True,
        "description": ("The owner's daily brief, written from today's mail, calendar, todos and deadlines "
                        "together. Use it when the owner asks for their rundown, brief or overview of the whole "
                        "day. One source alone (just the calendar, just the mail) is that app's own tool. It hands "
                        "the message to the rundown, which writes the brief itself."),
        "parameters": _NO_ARGS,
    },
    {
        "type": "function", "name": CLAUDE, "strict": True,
        "description": ("Connect this chat to a Claude Code session already running on the Mac, so what the owner "
                        "texts is typed into it and its replies come back here (on), or end that (off). on joins "
                        "the only running session, and otherwise sends the owner buttons to pick or start one."),
        "parameters": {"type": "object", "additionalProperties": False, "required": ["action", "session"],
                       "properties": {
                           "action": {"type": "string", "enum": ["on", "off"]},
                           "session": {"type": "string",
                                       "description": "The project folder the owner named, or empty."}}},
    },
    {
        "type": "function", "name": CODEX, "strict": True,
        "description": ("Codex, the coding agent, by name: open it in one of the owner's project folders and "
                        "chat with it here (\"open codex in eve\"). on starts it (with no folder, the owner picks "
                        "from a list), off closes it, status says which folder is connected."),
        "parameters": {"type": "object", "additionalProperties": False, "required": ["action", "folder"],
                       "properties": {
                           "action": {"type": "string", "enum": ["on", "off", "status"]},
                           "folder": {"type": "string",
                                      "description": "The project folder the owner named, or empty."}}},
    },
    {
        "type": "function", "name": STEALTH, "strict": True,
        "description": ("Stealth mode at the desk: on, eve acts asleep (quiet, face idle) until woken; off wakes "
                        "it. Use it when the owner asks eve to go quiet or sleep, or to wake up."),
        "parameters": {"type": "object", "additionalProperties": False, "required": ["on"],
                       "properties": {"on": {"type": "boolean", "description": "true: stealth on; false: wake."}}},
    },
    {
        "type": "function", "name": APPS, "strict": True,
        "description": ("Send the button that opens eve's Mini App, home of the apps eve made for the owner. "
                        "Use it when the owner wants to open or see those apps."),
        "parameters": _NO_ARGS,
    },
    {
        "type": "function", "name": LINK, "strict": True,
        "description": ("Send the link that pairs Eve Link, the owner's app on their phone or iPad, with eve: "
                        "for connecting, pairing or linking a device to Eve Link. new_link true makes a fresh "
                        "link and the old one stops working: only when the owner asks for a new one."),
        "parameters": {"type": "object", "additionalProperties": False, "required": ["new_link"],
                       "properties": {"new_link": {"type": "boolean"}}},
    },
]
TOOL_NAMES = tuple(t["name"] for t in TOOLS)

# Reading a file on the Mac (v4, 2026-10-09): list_files finds a file, send_file sends it, and nothing read one, so
# "what does the file say" went to a whole computer task. Read-only: the owner's home folder, no hidden folder,
# plain text only (telegram.read_file). Its result is outside text, so it taints the turn (tool_gate.py).
READ_FILE = "read_file"
FILE_TOOLS: list[dict[str, Any]] = [
    {
        "type": "function", "name": READ_FILE, "strict": True,
        "description": ("Read a plain-text file on the owner's Mac (a .txt, .md, .csv, .json, code or a log): its "
                        "text, up to about 12,000 characters. Home folder only; hidden folders are off limits. "
                        "list_files finds the path when the owner names the file only roughly."),
        "parameters": {"type": "object", "additionalProperties": False, "required": ["path"],
                       "properties": {"path": {"type": "string", "description": "Absolute path, or ~/..."}}},
    },
]
# The hand-offs: the command's handler sent the message, so the turn ends with the call (no second model call).
HANDS_OFF = frozenset({RUNDOWN, CLAUDE, CODEX, STEALTH, APPS, LINK})


def tools(environ: Optional[Mapping[str, str]] = None) -> list[dict[str, Any]]:
    """The menu's tools and read_file while the switch is on; none while it is off."""
    return list(TOOLS) + list(FILE_TOOLS) if enabled(environ) else []


def ended(result: Any) -> bool:
    """A hand-off's result that ends the turn: it worked, and its handler already said so in the chat."""
    return isinstance(result, dict) and bool(result.get("ok")) and bool(result.get("end_turn"))
