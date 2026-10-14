"""The owner golden: what full eve's brain is told and offered, how its Composio gate decides, the launchd plist it
is installed with, and what `eve-bridge daemon` builds, recorded once from main's code (WP-1 of
docs/plans/eve-lite.md).

eve lite (EVE_PROFILE=lite) lives inside files the owner's daemon also runs: telegram.py, composio_tools.py,
tool_gate.py, self_context.py, cli.py and _service_launchd.py. Every lite branch sits behind profile.is_lite(), and
with EVE_PROFILE unset the owner's eve stays byte for byte what it was. This tool computes those files' outputs for
owner-shaped environments and compares them with tests/golden/owner_main.json, which ``--write`` records only from
a tree whose bridge/src equals main's (the merge base of HEAD and origin/main):

* telegram.configured()'s fields, with ``about`` (self_context's block, commit line off) as a hash;
* request(): the instructions' hash, the tool names and the whole body's hash, for a Telegram turn and an app turn,
  with and without Composio's meta tools, each with the owner's lent parts (memory and its profile, the chat log,
  the watcher, the vault, the / menu's tools and their tool-choice lines, science search), and an app turn's body
  as the speed dial grades it (graded: instant, think);
* link_chat.app_block()'s hash, and self_context.lines(commit="");
* composio_tools.configured()'s fields and toolkit policy, and decide()'s verdict (with the owner's yes/no line)
  over a corpus drawn from test_composio_tools.py, test_composio_workbench.py and test_routine_app_policy.py,
  under four policies;
* tool_gate.source(), acting() and taints() for every tool name the brain offers somewhere;
* own_tools() and menu_tools.tools() names, each switch both ways, and telegram.TOOL_NAMES;
* _service_launchd's plists, with the home, the interpreter and the Claude homes fixed;
* `eve-bridge daemon` run through cli.main with a spy in daemon.Daemon's place: that it builds one, and with what.

The environments have the owner's shape: app-only with a token and an owner (EVE_TELEGRAM=app), and EVE_TELEGRAM=1,
each with and without Composio, with EVE_APPS=0 so no direct app signs in. Keys are placeholders that only say
"set", and the owner id is made up.

Hermetic, so the golden holds on any Mac and under any caller: each value is computed with os.environ replaced by
the environment named here and HOME a fresh temporary folder (holding only a lights file of the owner's shape), the
commit line off (self_context.running_commit), mem0 counted as installed, the watcher's Playwright and curl_cffi
rungs off (each is a package probe), and a process start through subprocess, os.system or os.posix_spawn refused
and recorded in the snapshot. Nothing reads the caller's environment, ~/.config/eve-bridge or the network.

    .venv/bin/python tools/owner_golden.py --check      # GOLDEN_OK, or each difference and exit 1
    .venv/bin/python tools/owner_golden.py --dump DIR   # the texts behind the hashes, to diff two trees
    .venv/bin/python tools/owner_golden.py --write      # refused unless bridge/src is main's code

``--check`` prints GOLDEN_OK only after the snapshot equals the golden and its positive control holds: the same
environments with the owner removed differ from the golden, in every environment's door and request. A deliberate
change to full eve on main is recorded again with ``--write`` in a worktree at origin/main; on a branch, --dump in
both trees shows what moved.
"""

from __future__ import annotations

import argparse
import contextlib
import dataclasses
import hashlib
import io
import json
import logging
import os
import subprocess
import sys
import tempfile
import traceback
from pathlib import Path
from typing import Any, Iterator, Mapping, Optional
from unittest import mock

from eve_bridge import (
    _service_launchd,
    cli,
    composio_tools,
    link_chat,
    mem0_memory,
    memory,
    menu_tools,
    science_search,
    second_brain,
    self_context,
    telegram,
    tool_gate,
    watch,
    websearch,
)
from eve_bridge import daemon as daemon_mod

BRIDGE = Path(__file__).resolve().parents[1]
GOLDEN = BRIDGE / "tests" / "golden" / "owner_main.json"
MARK = "GOLDEN_OK"

# ---- the owner's shape ------------------------------------------------------------------------------------------

SET = "golden-placeholder-not-a-key"   # a value that only says "set": never key-shaped (AGENTS rule 2)
OWNER = "4242"                          # a made-up numeric Telegram id
SAFE_PATH = "/usr/bin:/bin"
SERIAL_GLOB = "/dev/cu.usbmodem*"       # what AGENTS.md installs the service with

# The owner's env file as AGENTS.md and the docs describe it: OpenRouter for every model call (Jev's route too),
# TinyFish and Firecrawl for the search router, the Mini App, memory, the vault and file sharing.
OWNER_ENV: dict[str, str] = {
    "OPENROUTER_API_KEY": SET, "TINYFISH_API_KEY": SET, "FIRECRAWL_API_KEY": SET,
    "EVE_TELEGRAM_TOKEN": SET, "EVE_TELEGRAM_OWNER": OWNER, "EVE_JEV_ROUTE": "openrouter",
    "EVE_MINIAPP": "1", "EVE_MEMORY": "1", "EVE_SECOND_BRAIN": "1", "EVE_SHARE": "1",
    "EVE_APPS": "0",                                    # no direct app: nothing reads a sign-in or the network
    "EVE_WATCH_TLS": "0", "EVE_WATCH_BROWSER": "0",     # "auto" would probe for curl_cffi and Playwright
}
COMPOSIO_ENV: dict[str, str] = {"EVE_COMPOSIO": "1", "COMPOSIO_API_KEY": SET}
ENVS: dict[str, dict[str, str]] = {
    "app_only": {**OWNER_ENV, "EVE_TELEGRAM": "app"},
    "app_only_composio": {**OWNER_ENV, "EVE_TELEGRAM": "app", **COMPOSIO_ENV},
    "telegram_on": {**OWNER_ENV, "EVE_TELEGRAM": "1"},
    "telegram_on_composio": {**OWNER_ENV, "EVE_TELEGRAM": "1", **COMPOSIO_ENV},
}
# The owner's three brands of light, so self_context names them (lights.KINDS order).
LIGHTS = {"lights": [
    {"name": "floor lamp", "kind": "govee", "ip": "192.0.2.10", "device": "golden-govee", "sku": "H6008"},
    {"name": "desk bulb", "kind": "wiz", "ip": "192.0.2.11", "device": "golden-wiz", "sku": "ESP01"},
    {"name": "desk strip", "kind": "triones", "address": "golden-uuid"},
]}

# One turn's fixed parts. The developer note is lent (turn_context would read the clock), so the body is stable.
CONTEXT = "System context:\nThe clock is fixed for the owner golden."
ITEMS = [telegram.message_item("user", "what's on today?")]
PROFILE = "Name: Sam. Likes green tea. Prefers short answers."
TODAY = "(texted) owner: good morning\n(texted) eve: Morning!"
# Composio Connect's seven meta tools (docs.composio.dev, 2026-10-13), as Responses function tools.
COMPOSIO_NAMES = ("COMPOSIO_SEARCH_TOOLS", "COMPOSIO_GET_TOOL_SCHEMAS", "COMPOSIO_MULTI_EXECUTE_TOOL",
                  "COMPOSIO_MANAGE_CONNECTIONS", "COMPOSIO_WAIT_FOR_CONNECTIONS", "COMPOSIO_REMOTE_WORKBENCH",
                  "COMPOSIO_REMOTE_BASH_TOOL")
COMPOSIO_TOOLS = [{"type": "function", "name": n, "strict": False, "description": f"{n} (a stand-in schema).",
                   "parameters": {"type": "object", "properties": {}, "additionalProperties": True}}
                  for n in COMPOSIO_NAMES]

# ---- decide()'s corpus ------------------------------------------------------------------------------------------

M, W = composio_tools.MULTI_EXECUTE, composio_tools.WORKBENCH
# Shaped on the script from the owner's screenshot (test_composio_workbench.py): it only computes on fetched results.
CALENDAR_SCRIPT = """
import json
x = json.load(open('/mnt/files/mex/mine.json'))['items']
nlp = [e for e in x if 'natural language' in (e.get('summary') or '').lower()]
print(json.dumps([{'start': e['start'], 'where': e.get('location')} for e in nlp[:5]]))
"""


def _multi(*calls: tuple[str, Mapping[str, Any]], account: Optional[str] = "work") -> dict[str, Any]:
    """A multi-execute call; Gmail and the calendar name the owner's other account, as composio_tools wants."""
    items = []
    for slug, arguments in calls:
        item: dict[str, Any] = {"tool_slug": slug, "arguments": dict(arguments)}
        if account is not None:
            item["account"] = account
        items.append(item)
    return {"tools": items, "thought": "t", "sync_response_to_workbench": False}


def _bench(code: str) -> dict[str, Any]:
    return {"code_to_execute": code, "thought": "t", "current_step": "S", "session_id": "s"}


DECIDE_CASES: tuple[tuple[str, str, Any], ...] = (
    ("search tools", "COMPOSIO_SEARCH_TOOLS",
     {"queries": [{"use_case": "post a formatted message to slack channel", "known_fields": "channel_name:design"}]}),
    ("tool schemas", "COMPOSIO_GET_TOOL_SCHEMAS", {"tool_slugs": ["SLACK_SEND_MESSAGE"]}),
    ("connections, link", "COMPOSIO_MANAGE_CONNECTIONS", {"toolkits": ["gmail"]}),
    ("connections, two apps", "COMPOSIO_MANAGE_CONNECTIONS", {"toolkits": ["gmail", "slack"], "session_id": "s"}),
    ("connections, reinitiate", "COMPOSIO_MANAGE_CONNECTIONS", {"toolkits": ["gmail"], "reinitiate_all": True}),
    ("connections, wait", "COMPOSIO_WAIT_FOR_CONNECTIONS", {"toolkits": ["gmail"], "mode": "any"}),
    ("remote bash", "COMPOSIO_REMOTE_BASH_TOOL", {"command": "ls"}),
    ("workbench, no code", W, {}),
    ("workbench, computes", W, _bench(CALENDAR_SCRIPT)),
    ("workbench, pandas", W, _bench("import pandas as pd\ndf = pd.DataFrame([{'a': 1}])\nprint(df.describe())")),
    ("workbench, notion read", W,
     _bench("r = run_composio_tool('NOTION_SEARCH_NOTION_PAGE', {'query': 'trip'})\nprint(len(r['data']))")),
    ("workbench, gmail read", W, _bench("r = run_composio_tool('GMAIL_FETCH_EMAILS', {})\nprint(len(r))")),
    ("workbench, gmail send", W, _bench("run_composio_tool('GMAIL_SEND_EMAIL', {'to': 'reader@example.org'})")),
    ("workbench, drive upload", W, _bench("run_composio_tool(tool_slug='GOOGLEDRIVE_UPLOAD_FILE', arguments={})")),
    ("workbench, slack send", W, _bench("run_composio_tool('SLACK_SEND_MESSAGE', {'channel': 'design', 'text': 'hi'})")),
    ("workbench, unknown arguments", W, _bench("run_composio_tool('GOOGLEDRIVE_UPLOAD_FILE', arguments)")),
    ("workbench, network", W, _bench("import requests\nrequests.post('https://x.example', data='hi')")),
    ("workbench, aliased helper", W, _bench("f = run_composio_tool\nf('GMAIL_SEND_EMAIL', {})")),
    ("workbench, not python", W, _bench("this is not python (")),
    ("multi, no arguments", M, None),
    ("multi, opaque tools", M, {"tools": "opaque"}),
    ("multi, empty item", M, {"tools": [{}]}),
    ("multi, blank slug", M, {"tools": [{"tool_slug": ""}]}),
    ("multi, gmail read, no account", M, _multi(("GMAIL_FETCH_EMAILS", {}), ("SLACK_SEARCH_MESSAGES", {}), account=None)),
    ("multi, gmail read, personal", M, _multi(("GMAIL_FETCH_EMAILS", {}), account="Personal")),
    ("multi, gmail read, work", M, _multi(("GMAIL_FETCH_EMAILS", {"max_results": 5}))),
    ("multi, gmail label", M, _multi(("GMAIL_GET_LABEL", {"label_id": "INBOX"}))),
    ("multi, gmail send", M,
     _multi(("GMAIL_SEND_EMAIL", {"to": "reader@example.org", "subject": "Hello", "body": "Hi"}))),
    ("multi, gmail draft", M,
     _multi(("GMAIL_CREATE_EMAIL_DRAFT", {"to": "reader@example.org", "subject": "Hello", "body": "Hi"}))),
    ("multi, gmail read and delete", M, _multi(("GMAIL_GET_AND_DELETE_MESSAGE", {"message_id": "m1"}))),
    ("multi, calendar private event", M, _multi(("GOOGLECALENDAR_CREATE_EVENT", {"summary": "Focus"}))),
    ("multi, calendar invite", M,
     _multi(("GOOGLECALENDAR_CREATE_EVENT", {"summary": "Sync", "attendees": ["reader@example.org"]}))),
    ("multi, calendar quiet edit", M,
     _multi(("GOOGLECALENDAR_UPDATE_EVENT", {"event_id": "e1", "send_updates": "none"}))),
    ("multi, calendar edit", M, _multi(("GOOGLECALENDAR_UPDATE_EVENT", {"event_id": "e1"}))),
    ("multi, calendar read", M, _multi(("GOOGLECALENDAR_EVENTS_LIST_ALL_CALENDARS", {}))),
    ("multi, drive new file", M, _multi(("GOOGLEDRIVE_UPLOAD_FILE", {"file_to_upload": "notes.txt"}))),
    ("multi, drive into a folder", M,
     _multi(("GOOGLEDRIVE_UPLOAD_FILE", {"file_to_upload": "notes.txt", "folder_id": "shared"}))),
    ("multi, docs new document", M, _multi(("GOOGLEDOCS_CREATE_DOCUMENT", {"title": "Private draft"}))),
    ("multi, github private repo", M, _multi(("GITHUB_CREATE_REPOSITORY", {"name": "notes", "private": True}))),
    ("multi, slack read", M, _multi(("SLACK_SEARCH_MESSAGES", {"query": "design"}))),
    ("multi, slack send", M, _multi(("SLACK_SEND_MESSAGE", {"channel": "design", "text": "hi"}))),
    ("multi, notion read", M, _multi(("NOTION_FETCH_ROW", {"id": "p1"}))),
    ("multi, linear issue", M, _multi(("LINEAR_CREATE_LINEAR_ISSUE", {"title": "Login bug"}))),
    ("multi, mixed", M, _multi(("GOOGLECALENDAR_CREATE_EVENT", {"summary": "Focus"}),
                               ("SLACK_SEND_MESSAGE", {"channel": "design", "text": "hi"}),
                               ("GMAIL_SEND_EMAIL", {"to": "reader@example.org"}))),
    ("not a composio tool", "gmail__send_message", {"to": "reader@example.org"}),
)
# EVE_COMPOSIO_POLICY values each case is decided under ("" is the code's default, decide's policy=None).
POLICIES = {"default": "", "gmail_auto": "gmail=auto", "strict": "googlecalendar=ask,googledrive=read,slack=ask",
            "loose": "gmail=ask,slack=write,notion=write,linear=write"}
# Names beyond the brain's own lists whose tool_gate reading matters: direct apps, the apps' and doors' tools, and
# names nobody offers.
GATE_EXTRA = ("gmail__send_message", "calendar__list_events", "github__create_issue", "drive__search_files",
              "events_search", "opentable_search", "america_ask", "phone_location", "jobs_list", "take_on",
              "close_card", "lights_control", "spotify_control", "firetv_control", "camera_control", "meet_join",
              "capture_note", "COMPOSIO_SOMETHING_NEW", "a_tool_nobody_offers", "")

# The plist's inputs, fixed: no home path of anyone's (AGENTS rule 2).
PLIST_HOME = "/golden/home"
PLIST_PYTHON = "/golden/venv/bin/python3.12"
PLIST_WRAPPER = "/golden/wrapper-claude-home"

# What the golden must hold, section by section (test_owner_golden.py and --check read it).
SECTIONS = ("telegram", "requests", "app_block", "self_context", "composio", "tool_gate", "tools", "texts",
            "plist", "cli", "hermetic")


# ---- the sandbox ------------------------------------------------------------------------------------------------

def _sha(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _digest(text: str) -> dict[str, Any]:
    return {"sha256": _sha(text), "chars": len(text)}


def _canon(value: Any) -> str:
    """A request body as the bytes it is sent as, keys sorted. No fallback for a value JSON cannot hold: a set
    would be written in an order that changes from run to run."""
    return json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))


@contextlib.contextmanager
def _owner_home() -> Iterator[str]:
    """A fresh HOME holding only the owner's lights file, removed afterwards."""
    with tempfile.TemporaryDirectory(prefix="owner-golden-") as home:
        lights_file = Path(home, ".config", "eve-bridge", "lights.json")
        lights_file.parent.mkdir(parents=True)
        lights_file.write_text(json.dumps(LIGHTS), encoding="utf-8")
        yield home


@contextlib.contextmanager
def _sandbox(env: Mapping[str, str], spawned: list[str]) -> Iterator[None]:
    """os.environ is exactly ``env`` (PATH fixed unless named), the commit line is off, mem0 counts as installed,
    and a process start (subprocess, os.system, os.posix_spawn) is refused, its program's name recorded in
    ``spawned``: everything the snapshot reads is named in this file."""

    def refuse(program: Any) -> None:
        if isinstance(program, str):
            first = program.split()[:1]
        elif isinstance(program, (list, tuple)):
            first = list(program)[:1]
        else:
            first = [program]
        spawned.append(os.path.basename(os.fsdecode(first[0])) if first else "?")
        raise PermissionError("owner golden: no process starts while the snapshot is taken")

    class NoProcess:
        def __init__(self, args: Any, *_: Any, **__: Any) -> None:
            refuse(args)

    with mock.patch.dict(os.environ, {"PATH": SAFE_PATH, **env}, clear=True), \
            mock.patch.object(self_context, "running_commit", lambda: ""), \
            mock.patch.object(mem0_memory, "available", lambda: True), \
            mock.patch.object(subprocess, "Popen", NoProcess), \
            mock.patch.object(os, "system", refuse), \
            mock.patch.object(os, "posix_spawn", lambda path, *_, **__: refuse(path)), \
            mock.patch.object(os, "posix_spawnp", lambda path, *_, **__: refuse(path)):
        yield


def drop_owner(env: Mapping[str, str]) -> dict[str, str]:
    """The positive control's mutation: the same environment with no Telegram owner."""
    return {k: v for k, v in env.items() if k != "EVE_TELEGRAM_OWNER"}


# ---- the parts --------------------------------------------------------------------------------------------------

# TelegramConfig's fields on main, by name: a field added later (a default for every caller) is not a change.
TELEGRAM_FIELDS = ("enabled", "app_only", "token", "owner_ids", "model", "effort", "app_think_model",
                   "app_think_effort", "stale_secs", "ask_timeout_secs", "idle_close_secs", "ask_permissions",
                   "drafts", "search", "about")


def _telegram(cfg: telegram.TelegramConfig, texts: dict[str, str], key: str) -> dict[str, Any]:
    out: dict[str, Any] = {}
    for name in TELEGRAM_FIELDS:
        value = getattr(cfg, name)
        if name == "token":
            value = "<set>" if value else ""          # set or not: the value is the environment's, not code's
        elif name == "owner_ids":
            value = sorted(value)
        elif name == "search":
            value = dataclasses.asdict(value)
        elif name == "about":
            texts[f"about.{key}"] = value
            value = _digest(value)
        out[name] = value
    return out


def _lent(app_tools: list[dict[str, Any]]) -> dict[str, Any]:
    """request()'s keyword arguments for one of the owner's turns: a part of what TelegramInlet._turn_parts lends,
    in its order. Memory with a profile and today's talk, the chat log (recent_messages and its recall paragraph),
    the vault, and in the watch slot the / menu's tools, the watcher's and science search's, with the tool-choice
    lines for every name the turn offers."""
    memory_tools = list(memory.TOOLS)
    watch_tools = menu_tools.tools() + list(watch.TOOLS) + science_search.tools()
    offered = {str(t["name"]) for t in [*watch_tools, *memory_tools, *app_tools, *telegram.own_tools()]} | {
        websearch.TOOL_NAME, telegram.RECENT_MESSAGES_TOOL["name"], *second_brain.SECOND_BRAIN_TOOL_NAMES}
    choice = telegram.intent_block(offered) if menu_tools.enabled() else ""
    return {"profile": PROFILE, "app_tools": list(app_tools), "vault": True, "today": TODAY,
            "memory_tools": memory_tools, "watch_tools": watch_tools,
            "watch_block": "\n\n".join(b for b in (choice, watch.INSTRUCTIONS_BLOCK) if b),
            "recall": telegram.recall_line(True, True), "chat_tools": [telegram.RECENT_MESSAGES_TOOL]}


def _requests(cfg: telegram.TelegramConfig, texts: dict[str, str], key: str) -> dict[str, Any]:
    out: dict[str, Any] = {}
    app_block = link_chat.app_block()
    for turn, block in (("telegram_turn", ""), ("app_turn", app_block)):
        for suffix, app_tools in (("", []), ("_composio_tools", COMPOSIO_TOOLS)):
            body = telegram.request(cfg, list(ITEMS), context=CONTEXT, app_block=block, **_lent(app_tools))
            name = turn + suffix
            texts[f"requests.{key}.{name}"] = body["instructions"]
            out[name] = {"instructions": _digest(body["instructions"]),
                         "tools": telegram.offered_names(body["tools"]), "body_sha256": _sha(_canon(body))}
            if block:                               # the app's speed dial (telegram.GRADES); "auto" is the body
                out[name]["graded_sha256"] = {grade: _sha(_canon(telegram.graded(body, cfg, grade)))
                                              for grade in ("instant", "think")}
    return out


def _self_context() -> dict[str, Any]:
    lines = self_context.lines(commit="")
    return {"lines": lines, "sha256": _sha("\n".join(lines))}


def _composio(cfg: telegram.TelegramConfig, home: str) -> tuple[dict[str, Any], dict[str, str]]:
    cc = composio_tools.configured(owner_ids=cfg.owner_ids)
    fields = {"enabled": cc.enabled, "user_id": cc.user_id,
              "state_path": str(cc.state_path).replace(home, "~", 1), "timeout_secs": cc.timeout_secs,
              "toolkits": list(cc.toolkits) if cc.toolkits is not None else None}
    return fields, composio_tools.toolkit_policy()


def _decisions() -> dict[str, Any]:
    policies = {name: composio_tools.toolkit_policy({"EVE_COMPOSIO_POLICY": raw}) if raw else None
                for name, raw in POLICIES.items()}
    out: dict[str, Any] = {}
    for key, name, args in DECIDE_CASES:
        case: dict[str, Any] = {}
        for pname, policy in policies.items():
            d = composio_tools.decide(name, args, policy)
            case[pname] = [d.action, list(d.slugs), d.why]
        case["ask_line"] = composio_tools.describe_for_confirmation(name, args)
        out[key] = case
    return {"policies": policies, "cases": out}


def _tool_gate() -> dict[str, Any]:
    names = (set(telegram.TOOL_NAMES) | set(menu_tools.TOOL_NAMES) | {str(t["name"]) for t in menu_tools.FILE_TOOLS}
             | {str(t["name"]) for t in memory.TOOLS} | set(watch.TOOL_NAMES) | set(science_search.TOOL_NAMES)
             | {telegram.RECENT_MESSAGES_TOOL["name"], websearch.TOOL_NAME} | set(COMPOSIO_NAMES) | set(GATE_EXTRA))
    return {n: {"source": tool_gate.source(n), "acting": tool_gate.acting(n, {}), "taints": tool_gate.taints(n, {})}
            for n in sorted(names)}


def _names(tools: list[dict[str, Any]]) -> list[str]:
    return [str(t.get("name") or t.get("type")) for t in tools]


def _tools() -> dict[str, Any]:
    out = {"TOOL_NAMES": list(telegram.TOOL_NAMES), "own_tools": _names(telegram.own_tools()),
           "menu_tools": _names(menu_tools.tools())}
    with mock.patch.dict(os.environ, {"EVE_ROBOT": "1"}):
        out["own_tools_robot"] = _names(telegram.own_tools())
    with mock.patch.dict(os.environ, {"EVE_MENU_TOOLS": "off"}):
        out["menu_tools_off"] = _names(menu_tools.tools())
    return out


def plists(spawned: Optional[list[str]] = None) -> dict[str, Any]:
    """_service_launchd's units as `eve-bridge install --service` writes them, with HOME, the interpreter, the log
    paths and the Claude homes fixed: the serial glob AGENTS.md installs with, none, a wrapper's Claude home
    beside ~/.claude, and the notes widget. The golden's ``plist`` section; a test of the plist module compares
    this with it."""
    spawned = [] if spawned is None else spawned
    logs = {"LOG_PATH": Path(PLIST_HOME, "Library", "Logs", "eve-bridge.log"),
            "WIDGET_LOG_PATH": Path(PLIST_HOME, "Library", "Logs", "eve-bridge-notes-widget.log")}
    units = (("daemon_serial", {}, lambda: _service_launchd._build_plist(SERIAL_GLOB)),
             ("daemon_plain", {}, lambda: _service_launchd._build_plist()),
             ("daemon_wrapper_home", {"CLAUDE_CONFIG_DIR": PLIST_WRAPPER},
              lambda: _service_launchd._build_plist(SERIAL_GLOB)),
             ("widget", {}, _service_launchd._build_widget_plist))
    out: dict[str, Any] = {}
    for name, extra, build in units:
        with _sandbox({"HOME": PLIST_HOME, **extra}, spawned), mock.patch.object(sys, "executable", PLIST_PYTHON), \
                mock.patch.multiple(_service_launchd, **logs):
            xml = build().decode("utf-8")
        out[name] = {"sha256": _sha(xml), "xml": xml}
    return out


class _Built(Exception):
    """The spy's way out of cli.main: the command built its Daemon, and nothing after that runs here."""


def daemon_command(argv: Optional[list[str]] = None, *, spawned: Optional[list[str]] = None) -> dict[str, Any]:
    """`eve-bridge daemon` (or ``argv``) through cli.main, the owner's environment and serial glob, a fresh HOME and
    socket, with a spy in daemon.Daemon's place: whether the command built one, its arguments, and the exit code
    when it returned instead. The spy stands in at both names cli can reach it by: main's cli.py imports Daemon at
    the top, a lazy import reads daemon.Daemon when it runs."""
    spawned = [] if spawned is None else spawned
    seen: dict[str, Any] = {}

    class DaemonSpy:
        def __init__(self, *args: Any, **kwargs: Any) -> None:
            seen["args"], seen["kwargs"] = list(args), dict(kwargs)
            raise _Built

    exit_code: Any = None
    root = logging.getLogger()
    level, handlers = root.level, list(root.handlers)     # _run_daemon's basicConfig must not outlive the call
    with _owner_home() as home:
        sock = str(Path(home, "eve-bridge.sock"))
        args = ["daemon", "--socket", sock] if argv is None else list(argv)
        with contextlib.ExitStack() as stack:
            stack.enter_context(_sandbox({**ENVS["app_only"], "EVE_SERIAL_PORT": SERIAL_GLOB, "HOME": home}, spawned))
            stack.enter_context(mock.patch.object(daemon_mod, "Daemon", DaemonSpy))
            if hasattr(cli, "Daemon"):
                stack.enter_context(mock.patch.object(cli, "Daemon", DaemonSpy))
            stack.enter_context(contextlib.redirect_stdout(io.StringIO()))
            stack.enter_context(contextlib.redirect_stderr(io.StringIO()))
            try:
                exit_code = cli.main(args)
            except _Built:
                pass
            except SystemExit as e:
                exit_code = e.code
            finally:
                root.setLevel(level)
                root.handlers[:] = handlers
    kwargs = {k: ("<socket>" if v == sock else v) for k, v in seen.get("kwargs", {}).items()}
    return {"builds": f"{daemon_mod.__name__}.Daemon" if seen else None, "args": seen.get("args", []),
            "kwargs": kwargs, "exit": exit_code}


# ---- the snapshot -----------------------------------------------------------------------------------------------

def snapshot(envs: Optional[Mapping[str, Mapping[str, str]]] = None, *,
             texts: Optional[dict[str, str]] = None) -> dict[str, Any]:
    """Everything the golden holds, as JSON values. ``envs`` replaces the owner's environments by name (the
    positive control); the parts that read no environment are computed in ENVS["app_only"] whatever it says.
    ``texts`` collects the texts behind the hashes (--dump)."""
    envs = ENVS if envs is None else envs
    texts = {} if texts is None else texts
    spawned: list[str] = []
    out: dict[str, Any] = {"telegram": {}, "requests": {}, "self_context": {},
                           "composio": {"configured": {}, "policy": {}}}
    for key, env in envs.items():
        with _owner_home() as home, _sandbox({**env, "HOME": home}, spawned):
            cfg = telegram.configured()
            out["telegram"][key] = _telegram(cfg, texts, key)
            out["requests"][key] = _requests(cfg, texts, key)
            out["self_context"][key] = _self_context()
            out["composio"]["configured"][key], out["composio"]["policy"][key] = _composio(cfg, home)
    with _owner_home() as home, _sandbox({**ENVS["app_only"], "HOME": home}, spawned):
        block = link_chat.app_block()
        texts["app_block"] = block
        out["app_block"] = _digest(block)
        out["tools"] = _tools()
        out["tool_gate"] = _tool_gate()
        out["composio"]["decide"] = _decisions()
        out["texts"] = {"CALL_NOTE": _digest(telegram.CALL_NOTE),
                        "INSTRUCTIONS_ROBOT": _digest(telegram.INSTRUCTIONS_ROBOT)}
    out["plist"] = plists(spawned)
    out["cli"] = {"daemon": daemon_command(spawned=spawned)}
    out["hermetic"] = {"processes": sorted(set(spawned))}
    return json.loads(json.dumps(out))


def _flat(value: Any, prefix: str = "") -> dict[str, Any]:
    if isinstance(value, dict) and value:
        out: dict[str, Any] = {}
        for k, v in value.items():
            out.update(_flat(v, f"{prefix}.{k}" if prefix else str(k)))
        return out
    return {prefix: value}


def _short(value: Any, limit: int = 160) -> str:
    text = json.dumps(value, ensure_ascii=False)
    return text if len(text) <= limit else text[:limit - 1] + "…"


def _change(was: Any, now: Any) -> str:
    if isinstance(was, list) and isinstance(now, list) and all(isinstance(v, str) for v in was + now):
        added, removed = [v for v in now if v not in was], [v for v in was if v not in now]
        if added or removed:
            return f"added {_short(added)}, removed {_short(removed)}"
        return f"reordered: golden {_short(was)} now {_short(now)}"
    return f"golden {_short(was)} now {_short(now)}"


def differences(want: Mapping[str, Any], got: Mapping[str, Any]) -> list[str]:
    """One line per value that differs, by its dotted path: changed, gone, or not in the golden."""
    a, b = _flat(dict(want)), _flat(dict(got))
    out = []
    for path in sorted(set(a) | set(b)):
        if path not in b:
            out.append(f"{path}: gone (golden {_short(a[path])})")
        elif path not in a:
            out.append(f"{path}: not in the golden ({_short(b[path])})")
        elif a[path] != b[path]:
            out.append(f"{path}: {_change(a[path], b[path])}")
    return out


def control_misses(want: Mapping[str, Any]) -> list[str]:
    """The positive control: the owner's environments with no owner must differ from ``want`` in each
    environment's door (telegram.<env>.enabled) and its request bodies. What it failed to catch, or []."""
    caught = differences(want, snapshot({key: drop_owner(env) for key, env in ENVS.items()}))
    misses = []
    for key in ENVS:
        for path in (f"telegram.{key}.enabled:", f"requests.{key}."):
            if not any(line.startswith(path) for line in caught):
                misses.append(path.rstrip(":."))
    return misses


# ---- the golden file --------------------------------------------------------------------------------------------

def load(path: Path = GOLDEN) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _json(value: Any, indent: int = 0) -> str:
    """Sorted keys one per line, every list on one line: a golden that reads and diffs line by line."""
    if isinstance(value, dict) and value:
        inner = " " * (indent + 1)
        items = [f"{inner}{json.dumps(k, ensure_ascii=False)}: {_json(v, indent + 1)}" for k, v in sorted(value.items())]
        return "{\n" + ",\n".join(items) + "\n" + " " * indent + "}"
    return json.dumps(value, ensure_ascii=False)


def _main_base() -> tuple[str, str]:
    """(main's commit this tree forked from, why it may not record a golden, or "")."""
    root = BRIDGE.parent

    def git(*args: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(["git", "-C", str(root), *args], capture_output=True, text=True, check=False)

    base = git("merge-base", "HEAD", "origin/main")
    sha = base.stdout.strip()
    if base.returncode != 0 or not sha:
        return "", "this checkout has no origin/main to record main's code from"
    short = git("rev-parse", "--short=7", sha).stdout.strip() or sha[:7]
    if git("diff", "--quiet", sha, "--", "bridge/src").returncode != 0:
        return short, (f"bridge/src differs from {short} (main, where this branch forked): the golden is recorded "
                       "from main's code only, never to make a change pass")
    return short, ""


def write(path: Path = GOLDEN, *, force: bool = False) -> int:
    base, why = _main_base()
    if why and not force:
        print(f"refused: {why}", file=sys.stderr)
        return 1
    doc = {"about": "Recorded by bridge/tools/owner_golden.py --write from main's code at `base`, with EVE_PROFILE "
                    "unset. Recorded again only for a deliberate change to full eve on main; never to make a lite "
                    "change pass.",
           "base": base, "snapshot": snapshot()}
    text = _json(doc) + "\n"
    if json.loads(text) != json.loads(json.dumps(doc)):
        raise AssertionError("the golden's writer lost a value")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")
    print(f"wrote {path} ({len(text)} bytes, base {base})")
    return 0


def check(path: Path = GOLDEN) -> int:
    """GOLDEN_OK only when every section is there, the snapshot equals it, and the positive control is caught."""
    try:
        golden = load(path)
        want = golden["snapshot"]
        if not isinstance(want, dict):
            raise TypeError("its snapshot is not an object")
    except (OSError, ValueError, KeyError, TypeError) as e:
        print(f"FAIL: cannot read the golden {path}: {type(e).__name__}: {e}")
        return 1
    problems = [f"the golden has no {name} section" for name in SECTIONS if not want.get(name)]
    try:
        diffs = differences(want, snapshot())
        misses = control_misses(want)
    except Exception as e:  # noqa: BLE001 — a part that cannot be computed is a failure to report, not a crash
        print(traceback.format_exc(), end="")
        print(f"FAIL: the snapshot could not be taken ({type(e).__name__}: {e})")
        return 1
    problems += [f"full eve changed: {line}" for line in diffs[:40]]
    if len(diffs) > 40:
        problems.append(f"... and {len(diffs) - 40} more differences (--dump DIR shows the texts)")
    problems += [f"positive control: the owner removed was not caught at {miss}" for miss in misses]
    for line in problems:
        print("FAIL:", line)
    if problems:
        return 1
    print(f"{MARK} {len(_flat(want))} values equal the golden from {golden.get('base') or 'main'}; "
          f"the owner removed was caught in all {len(ENVS)} environments")
    return 0


def dump(folder: Path) -> int:
    texts: dict[str, str] = {}
    snapshot(texts=texts)
    folder.mkdir(parents=True, exist_ok=True)
    for key, text in sorted(texts.items()):
        (folder / f"{key}.txt").write_text(text, encoding="utf-8")
    print(f"wrote {len(texts)} texts to {folder}")
    return 0


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(prog="owner_golden.py", description=__doc__.split("\n\n")[0])
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true", help="compare full eve with the golden; GOLDEN_OK or exit 1")
    mode.add_argument("--write", action="store_true", help="record the golden (only from main's code)")
    mode.add_argument("--dump", metavar="DIR", type=Path, help="write the texts behind the hashes to DIR")
    parser.add_argument("--golden", type=Path, default=GOLDEN, help=argparse.SUPPRESS)
    parser.add_argument("--force", action="store_true", help=argparse.SUPPRESS)
    args = parser.parse_args(argv)
    logging.basicConfig(level=logging.ERROR, format="%(levelname)s %(name)s: %(message)s")
    if args.write:
        return write(args.golden, force=args.force)
    if args.dump:
        return dump(args.dump)
    return check(args.golden)


if __name__ == "__main__":
    sys.exit(main())
