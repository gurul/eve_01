"""The Work tab's sessions: every Claude Code session on this Mac, talked to from the phone, and new ones eve starts
in ``~/Documents/personal/work``.

Owner, 2026-10-12: "can work essentially be a new terminal session in documents personal", "since its long
horizon", "make a folder called work under personal where this happens". Owner, 2026-10-26: "I want to also be able
to talk to current terminal sessions through the work area ... Unify everything. This is important." A new session is
plain ``claude --dangerously-skip-permissions`` in that one folder (``WORK_DIR``; ``EVE_WORK_ROOT`` moves it), never
claude_launch's "work" *area* (``~/Documents/work`` with era-code). The routes are link_chat.py's
``/api/link/sessions*``; the whole design is in docs/work-sessions.md.

**Start.** ``start(task, client_id)`` mints the session's id (a UUID), writes the task to ``<sid>.task`` and eve's
MCP config to ``<sid>.mcp.json`` (both 0600, in ``~/.config/eve-bridge/work/``, 0700), and opens one Terminal.app
window whose only line is code-made: ``cd <WORK_DIR> && exec <python> -P -m eve_bridge.work_sessions launch <sid>``
(``-P``: the shared work folder, where sessions write files, is never on ``sys.path``; the MCP server's args carry it
too). That line is passed to osascript as an argument, never written into the script's source. ``launch`` (``main``)
reads and deletes the task file and replaces itself with ``claude``, the task as one argv element after ``--``
(``launch_argv``): the task never passes through a shell or an AppleScript string, whatever it holds. The same
``client_id`` within ``DEDUPE_SECS`` answers the first start's session; nothing else opens.

**The list** is every live interactive Claude Code session, wherever it runs: Claude Code's own session registry
(claude_peer.live_sessions(): ``~/.claude/sessions/<pid>.json``, one per interactive session, with its messaging
socket when it has one), the daemon's hook state (``state.sessions``) and eve's own records, merged by session id.
Each row says where it runs (``where``: Warp, Terminal, VS Code..., the first app among the claude process's
ancestors in ``ps``). A session's state: waiting on you (a Notification that waits, or an AskUserQuestion), working
(between a prompt and Stop, or "busy" in the registry), idle, or starting (opened, no process yet).

**Which session is which.** ``claude --session-id <sid>`` (a new session) or ``claude --resume <sid>`` (one reopened)
makes Claude Code's own session id the one eve keeps, so the hooks' ``session_id`` (``note``) and ``ps``
(``session_pids``) both name it. ``ps`` finding the process is liveness and the stop target. A ``/clear`` inside the
session goes on under a new session id in the same process: the row stays (ps still names ours) and replies still
reach that terminal, its state and detail stop following, and the new id is listed beside it.

**Reply** reaches any live session the same way, whatever it runs in: claude_peer.send puts the owner's words in the
session's own messaging socket, a cross-session message whose first line (claude_peer.OWNER_LINE) says they are the
owner's, typed on the phone. An idle session wakes on it; a busy one takes it at its next tool round, so a second and
third send just queue. A session that holds messages from other sessions for its user's approval answers "held", and
the phone says it waits on the Mac; eve never dresses a message up to get past that. Only a session with no usable
socket (an older Claude Code) that eve itself started in Terminal is typed into instead: Terminal.app's ``do script
<text> in <tab>`` into the busy tab whose tty is the claude process's tty now (``ps``), looked for in the window eve
recorded first and then in every window, because recorded window ids and ttys both went stale (2026-10-26). A live
session is never forked, and no window is opened to talk to it.

**Resume** (``continue_session``; the route and the app's ``can_continue`` keep the old name): a live session takes
the words as a reply. One that has ended reopens as itself: ``claude --resume <sid>`` (never ``--fork-session``) in a
new Terminal window in the folder it started in, the words its first prompt, so the thread stays one session with one
id and one transcript; eve keeps a record under that id, so reply, stop and the row work on it as on any Work session.
Forking a live session made a second agent redo the first one's unfinished work in the same repository, twice, on
2026-10-26.

**Ended, or not sure.** An eve session has ended once ``ps`` no longer finds its claude after ``GRACE_SECS``. Any
other session has ended once Claude Code's registry no longer lists it, but only when the registry is complete (every
claude process in ``ps`` is in it, an IDE's and a ``claude -p`` too; only Chrome's native host is not a session): an
older Claude Code that never registers would otherwise look ended while it runs, and reopening a running session is
the one thing this must not do. A one-shot ``claude -p`` (a hook's drafter) registers too; the list leaves it out. When eve cannot tell, the session
stays listed, read-only. An eve session that ended can run again (the owner resumed it at the Mac): from then on it is
judged, listed and sent to like any other session, and it is not eve's to stop.

**Stop.** SIGTERM to the pid ``ps`` gives for the session, SIGKILL after ``STOP_WAIT_SECS``. Idempotent. Only eve's
own sessions (started or reopened by eve) can be stopped.

**Detail** reads the transcript's last ``TAIL_BYTES`` (growing to ``MAX_TAIL_BYTES``) on a thread: the owner's prompts,
the owner's words eve relayed (without eve's envelope and first line) and Claude's text, including its SendMessage
answers to eve; never tool calls, tool results, thinking or other sessions' messages, each clipped to ``TURN_CHARS``
with token-shaped strings and ``KEY=value`` secrets masked (``redact``). Folders are basenames only.

Logs carry ids' first 8 characters, counts and states, never a task, a reply or a transcript line.
"""
from __future__ import annotations

import asyncio
import contextlib
import hashlib
import hmac
import json
import logging
import os
import re
import secrets
import shlex
import shutil
import signal
import subprocess
import sys
import time
import uuid
from dataclasses import asdict, dataclass
from datetime import datetime
from pathlib import Path
from typing import Any, AsyncIterator, Awaitable, Callable, Iterable, Optional

from . import claude_live, claude_peer
from .state import notification_waits

log = logging.getLogger(__name__)

CONFIG_DIR = Path.home() / ".config" / "eve-bridge"
REGISTRY_NAME = "work-sessions.json"
FILES_NAME = "work"                       # <sid>.task and <sid>.mcp.json, 0600, in a 0700 folder
UUID_RE = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")
MAX_TASK_CHARS = 4000
MAX_REPLY_CHARS = 4000
MAX_CLIENT_ID_CHARS = 64
TITLE_CHARS = 80
LINE_CHARS = 200
TURN_CHARS = 2000
TURNS_SHOWN = 20
TAIL_BYTES = 512 * 1024                  # detail reads this much of the transcript's end first
MAX_TAIL_BYTES = 8 * 1024 * 1024         # and doubles it, up to this, while it has fewer turns than it shows
LINE_TAIL_BYTES = 64 * 1024              # the list's last line, from at most this much
DEDUPE_SECS = 600.0                      # the same client_id within this is the same session (or the same send)
GRACE_SECS = 30.0                        # a session this new is "starting" whatever ps says
STARTING_WAIT_SECS = 15.0                # a send to an eve session still opening waits this long for its socket
POLL_SECS = 0.5
STOP_WAIT_SECS = 5.0                     # SIGTERM, then SIGKILL after this
KEEP_SECS = 7 * 24 * 3600                # an ended session's record is kept this long
MAX_LISTED = 30
MAX_ANCESTORS = 40                       # how far up the process tree ``where`` looks
STATES = ("starting", "working", "waiting", "idle")      # live states; "ended" only in detail
ENDED_CODE = "ended"                     # the "code" beside ENDED, so the app needs no display text to match
OFF_LINE = "Work sessions are off on this Mac (EVE_WORK_SESSIONS=0)."
ENDED = "that session has ended"
NO_WINDOW = "can't reach that session's window"
NOT_OURS = "eve can only stop sessions it started or reopened from Work"
NO_SOCKET = ("that session can't take messages from eve: its Claude Code has no message socket. Update Claude Code "
             "there, or answer it at the Mac")
HELD = "Sent. That session asks before it takes messages from other sessions, so it waits for your approval on the Mac."
REFUSED = "that session refused the message"
UNREACHABLE = "eve couldn't reach that session just now; try again"
STARTING = "that session is still starting; try again in a moment"
UNSURE = "eve can't tell whether that session is still running, so it won't reopen it"
NO_CONTINUE = "eve can't find that session's folder or transcript, so it can't reopen it"
UNTRUSTED = ("Claude hasn't been trusted in that folder on this Mac yet, so it would stop at the trust prompt. "
             "Open Claude there once and accept it.")
UNKNOWN = "there is no such session"

# Terminal.app, with every value an argument (``on run argv``), never part of the script's text.
# Opens one window running item 1, and answers "<window id> <tty>" for the tab it made.
# "0" for a window id it could not read: the window is open, and a reply looks for its tab in every window.
TERMINAL_OPEN = '''on run argv
  tell application "Terminal"
    set t to do script (item 1 of argv)
    activate
    set wid to 0
    try
      repeat with w in windows
        if (tabs of w) contains t then
          set wid to id of w
          exit repeat
        end if
      end repeat
    end try
    if wid is 0 then
      try
        set wid to id of front window
      end try
    end if
    set tt to ""
    try
      set tt to tty of t
    end try
    return (wid as string) & " " & tt
  end tell
end run'''
# Types item 3 into the tab whose tty is item 2, only while a program holds it (busy): window item 1 first (0: none
# recorded), then every window. A closed tab's window can keep showing a tty the system has since given to a new
# window, so a tab with that tty that is not busy is passed over, never typed into. Answers "ok <its window id>",
# "idle" (only such tabs) or "gone" (no tab has that tty).
TERMINAL_TYPE = '''on run argv
  set wid to (item 1 of argv) as integer
  set want to item 2 of argv
  tell application "Terminal"
    set found to "gone"
    set ws to {}
    if wid > 0 then
      try
        set ws to {window id wid}
      end try
    end if
    try
      set ws to ws & (every window)
    end try
    repeat with w in ws
      try
        repeat with t in (tabs of w)
          if (tty of t) is want then
            if busy of t then
              do script (item 3 of argv) in t
              return "ok " & ((id of w) as string)
            end if
            set found to "idle"
          end if
        end repeat
      end try
    end repeat
    return found
  end tell
end run'''


def enabled(environ: Any = None) -> bool:
    env = os.environ if environ is None else environ
    return str(env.get("EVE_WORK_SESSIONS", "1")).strip().lower() not in ("0", "off", "false", "no")


def work_root(environ: Any = None) -> Path:
    """``~/Documents/personal/work``, or ``EVE_WORK_ROOT``. Never claude_launch's work area."""
    env = os.environ if environ is None else environ
    raw = str(env.get("EVE_WORK_ROOT", "")).strip()
    return Path(raw).expanduser() if raw else Path.home() / "Documents" / "personal" / "work"


def files_dir(home: Optional[Path] = None) -> Path:
    return (home or CONFIG_DIR) / FILES_NAME


def flat(text: Any, limit: int) -> str:
    """One line, at most ``limit`` characters (an ellipsis when cut)."""
    line = " ".join(str(text or "").split())
    return line if len(line) <= limit else line[: max(0, limit - 1)].rstrip() + "…"


def clip(text: Any, limit: int) -> str:
    text = str(text or "").strip()
    return text if len(text) <= limit else text[: max(0, limit - 1)].rstrip() + "…"


# Token-shaped strings and KEY=value secrets, masked before a transcript line reaches the phone.
_SECRET_PATTERNS = (
    re.compile(r"\b(?:sk|pk|rk)-[A-Za-z0-9_-]{16,}"),                         # API keys (OpenAI, OpenRouter, ...)
    re.compile(r"\bgh[pousr]_[A-Za-z0-9]{20,}"),                               # GitHub tokens
    re.compile(r"\bgithub_pat_[A-Za-z0-9_]{20,}"),
    re.compile(r"\bxox[abprs]-[A-Za-z0-9-]{10,}"),                             # Slack
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),                                       # AWS access key ids
    re.compile(r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}"),   # JWTs
    re.compile(r"(?i)\bbearer\s+[A-Za-z0-9._~+/=-]{16,}"),
    re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z ]*PRIVATE KEY-----|$)"),
)
_KEY_VALUE = re.compile(r"(?im)\b([A-Z0-9_]*(?:KEY|TOKEN|SECRET|PASSWORD|PASSWD)[A-Z0-9_]*)\s*[=:]\s*(\"[^\"\n]*\"|'[^'\n]*'|\S+)")
_LONG_RANDOM = re.compile(r"\b(?=[A-Za-z0-9_-]*\d)(?=[A-Za-z0-9_-]*[A-Za-z])[A-Za-z0-9_-]{40,}\b")
MASK = "[hidden]"


def redact(text: str) -> str:
    out = text
    for pattern in _SECRET_PATTERNS:
        out = pattern.sub(MASK, out)
    out = _KEY_VALUE.sub(lambda m: f"{m.group(1)}={MASK}", out)
    return _LONG_RANDOM.sub(MASK, out)


# ---- transcripts -------------------------------------------------------------------------------------------------

def derived_transcript(cwd: Path, sid: str, config_dir: Optional[Path] = None) -> Path:
    """Where Claude Code writes a session's transcript: <config>/projects/<cwd, every character that is not a letter
    or a digit as "-">/<sid>.jsonl."""
    from .claude_home import claude_config_dir

    base = config_dir or claude_config_dir()
    return base / "projects" / re.sub(r"[^A-Za-z0-9]", "-", str(cwd)) / f"{sid}.jsonl"


def found_transcript(cwd: Optional[str], sid: str, config_dir: Optional[Path] = None) -> Optional[Path]:
    """A session's transcript when nothing recorded its path (a session the daemon adopted from a tool hook after a
    restart, or one only Claude Code's registry knows): the derived path, else ``<sid>.jsonl`` in any project folder.
    None when there is none."""
    from .claude_home import claude_config_dir

    if not UUID_RE.match(sid):
        return None
    if cwd:
        path = derived_transcript(Path(cwd), sid, config_dir)
        if path.is_file():
            return path
    base = (config_dir or claude_config_dir()) / "projects"
    with contextlib.suppress(OSError):
        for folder in base.iterdir():
            path = folder / f"{sid}.jsonl"
            if path.is_file():
                return path
    return None


def _epoch(stamp: Any) -> float:
    if not isinstance(stamp, str) or not stamp:
        return 0.0
    try:
        return datetime.fromisoformat(stamp.replace("Z", "+00:00")).timestamp()
    except ValueError:
        return 0.0


def _texts(content: Any, role: str) -> list[str]:
    """The words in one message's content: plain text only (no tool_use, tool_result, thinking or images)."""
    if isinstance(content, str):
        return [content]
    out: list[str] = []
    if isinstance(content, list):
        for block in content:
            if isinstance(block, dict) and block.get("type") == "text" and isinstance(block.get("text"), str):
                out.append(block["text"])
    return out


# A cross-session message as Claude Code files it: the envelope, a newline, the sender's text, a newline, the end tag
# (a session's Claude sees it inside a user line that says another session sent it).
_ENVELOPE = re.compile(r"<cross-session-message\b([^>]*)>\n?(.*?)\n?</cross-session-message>", re.S)
_ATTR = re.compile(r'([a-z][a-z-]*)="([^"]*)"')
REPLY_TOOL = "SendMessage"                # how a session answers another one


def _envelopes(text: str) -> list[tuple[dict[str, str], str]]:
    """Each cross-session message in ``text``: (its envelope's attributes, the sender's text)."""
    return [(dict(_ATTR.findall(m.group(1))), m.group(2)) for m in _ENVELOPE.finditer(text)]


def _from_eve(attrs: dict[str, str]) -> bool:
    return attrs.get("from-name") == claude_peer.EVE_NAME


def owner_words(body: str) -> str:
    """What the owner typed, from a message eve relayed: its first line (claude_peer.OWNER_LINE) left out."""
    body = body.strip()
    if body.startswith(claude_peer.OWNER_LINE):
        body = body[len(claude_peer.OWNER_LINE):]
    return body.strip()


def read_tail(path: Path, limit: int) -> tuple[list[dict[str, Any]], bool]:
    """The JSON objects in the last ``limit`` bytes of a JSONL file, and whether earlier lines were left out."""
    try:
        with open(path, "rb") as f:
            f.seek(0, os.SEEK_END)
            size = f.tell()
            start = max(0, size - limit)
            f.seek(start)
            raw = f.read(limit)
    except OSError:
        return [], False
    lines = raw.split(b"\n")
    if start > 0 and lines:
        lines = lines[1:]                          # the first line was cut in half
    out: list[dict[str, Any]] = []
    for line in lines:
        if not line.strip():
            continue
        try:
            obj = json.loads(line)
        except ValueError:
            continue
        if isinstance(obj, dict):
            out.append(obj)
    return out, start > 0


def transcript_turns(path: Optional[Path], *, limit: int = TURNS_SHOWN, tail: int = TAIL_BYTES,
                     chars: int = TURN_CHARS, grow_to: int = MAX_TAIL_BYTES,
                     eve: Iterable[str] = ()) -> tuple[list[dict[str, Any]], bool]:
    """The last ``limit`` turns: ``{"role": "owner" | "claude", "text", "at"}``, oldest first, and whether more came
    before. The owner's typed prompts (and those typed while Claude worked, which Claude Code files as queued_command
    attachments), the owner's words eve relayed (``_turns_in_tail``) and Claude's text only: subagents' lines, meta
    lines, command and system wrappers (text that starts with "<"), other sessions' messages, tool calls, tool results
    and thinking are left out. ``eve``: addresses eve's inbox has had, beside those the transcript's own envelopes
    name. A tail with fewer than ``limit`` turns is doubled up to ``grow_to``: a turn's attachments and tool results can
    fill 512 KB, which hid the owner's own prompt (a fork of a long session, 2026-10-26)."""
    if path is None:
        return [], False
    eve = tuple(eve)
    while True:
        turns, cut = _turns_in_tail(path, tail, chars, eve)
        if len(turns) >= limit or not cut or tail >= grow_to:
            return turns[-limit:], cut or len(turns) > limit
        tail = min(tail * 2, grow_to)


def _turns_in_tail(path: Path, tail: int, chars: int, eve: Iterable[str] = ()) -> tuple[list[dict[str, Any]], bool]:
    """The turns in the transcript's last ``tail`` bytes. eve's own messages are the owner's turns: Claude Code files
    one as a meta user line ("Another Claude session sent a message: <cross-session-message from-name="eve-phone">
    ...") or, taken mid-turn, as a queued_command attachment, and only the owner's words are kept. A SendMessage call
    to eve (to one of eve's addresses, or its name) is Claude's turn: that is how a session answers a message."""
    objs, cut = read_tail(path, tail)
    to_eve = {claude_peer.EVE_NAME, *(a for a in eve if a)}
    for obj in objs:                                  # the addresses eve's messages came from, in this transcript
        for _, text in _relayed(obj):
            for attrs, _ in _envelopes(text):
                if _from_eve(attrs) and attrs.get("from"):
                    to_eve.add(attrs["from"])
    turns: list[dict[str, Any]] = []

    def add(role: str, text: str, stamp: Any) -> None:
        turns.append({"role": role, "text": clip(redact(text), chars), "at": int(_epoch(stamp))})

    for obj in objs:
        if obj.get("isSidechain"):
            continue
        relayed = _relayed(obj)
        if relayed:
            for stamp, text in relayed:
                for attrs, body in _envelopes(text):
                    words = owner_words(body) if _from_eve(attrs) else ""
                    if words:
                        add("owner", words, stamp)
            continue
        if obj.get("isMeta"):
            continue
        queued = obj.get("attachment") if obj.get("type") == "attachment" else None
        if isinstance(queued, dict) and queued.get("type") == "queued_command":
            text = queued.get("prompt")
            if (queued.get("commandMode", "prompt") == "prompt" and isinstance(text, str) and text.strip()
                    and not text.strip().startswith("<")):
                add("owner", text.strip(), queued.get("timestamp") or obj.get("timestamp"))
            continue
        msg = obj.get("message") if isinstance(obj.get("message"), dict) else None
        if msg is None:
            continue
        role = msg.get("role")
        if role not in ("user", "assistant") or obj.get("type") not in (None, "user", "assistant"):
            continue
        content = msg.get("content")
        for text in _texts(content, role):
            text = text.strip()
            if not text or (role == "user" and text.startswith("<")):
                continue
            add("owner" if role == "user" else "claude", text, obj.get("timestamp"))
        if role == "assistant" and isinstance(content, list):
            for block in content:
                answer = _answer_to(block, to_eve)
                if answer:
                    add("claude", answer, obj.get("timestamp"))
    return turns, cut


def _relayed(obj: dict[str, Any]) -> list[tuple[Any, str]]:
    """(timestamp, text) for each cross-session message one transcript line holds: a meta user line, or a
    queued_command attachment, whose text has a ``<cross-session-message>``. Empty for any other line."""
    out: list[tuple[Any, str]] = []
    if obj.get("type") == "attachment":
        queued = obj.get("attachment")
        if isinstance(queued, dict) and queued.get("type") == "queued_command":
            text = queued.get("prompt")
            if isinstance(text, str) and "<cross-session-message" in text:
                out.append((queued.get("timestamp") or obj.get("timestamp"), text))
        return out
    msg = obj.get("message") if isinstance(obj.get("message"), dict) else None
    if obj.get("type") != "user" or not obj.get("isMeta") or msg is None or msg.get("role") != "user":
        return out
    for text in _texts(msg.get("content"), "user"):
        if "<cross-session-message" in text:
            out.append((obj.get("timestamp"), text))
    return out


def _answer_to(block: Any, to_eve: set[str]) -> str:
    """The text of a SendMessage call addressed to eve, else ""."""
    if not isinstance(block, dict) or block.get("type") != "tool_use" or block.get("name") != REPLY_TOOL:
        return ""
    args = block.get("input") if isinstance(block.get("input"), dict) else {}
    if not ({args.get("to"), args.get("recipient")} & to_eve):
        return ""
    for key in ("message", "content", "text"):
        if isinstance(args.get(key), str) and args[key].strip():
            return args[key].strip()
    return ""


def transcript_cwd(path: Optional[Path], limit: int = LINE_TAIL_BYTES) -> str:
    """The folder a session started in, from the first transcript line that names one. A hook's ``cwd`` is where the
    session's shell is now (``cd rust`` moves it), so this is the project, for the row and for a resume."""
    if path is None:
        return ""
    try:
        with open(path, "rb") as f:
            head = f.read(limit)
    except OSError:
        return ""
    for raw in head.split(b"\n"):
        try:
            obj = json.loads(raw)
        except ValueError:
            continue
        cwd = obj.get("cwd") if isinstance(obj, dict) else None
        if isinstance(cwd, str) and cwd.startswith("/"):
            return cwd
    return ""


def folder_trusted(cwd: str, config_dir: Optional[Path] = None) -> Optional[bool]:
    """Whether Claude Code's folder-trust prompt is already accepted for ``cwd`` or a folder above it (its state file
    keeps ``projects[<path>].hasTrustDialogAccepted``). An interactive session opens on that prompt in a folder that
    is not, before it reads its first prompt. None when the state file cannot be read: the caller does not block."""
    from .claude_home import claude_config_dir

    base = config_dir or claude_config_dir()
    state = Path.home() / ".claude.json" if base == Path.home() / ".claude" else base / ".claude.json"
    try:
        projects = json.loads(state.read_text(encoding="utf-8")).get("projects")
    except (OSError, ValueError, AttributeError):
        return None
    if not isinstance(projects, dict):
        return None
    for folder in (Path(cwd), *Path(cwd).parents):
        entry = projects.get(str(folder))
        if isinstance(entry, dict) and entry.get("hasTrustDialogAccepted") is True:
            return True
    return False


def last_line(path: Optional[Path], eve: Iterable[str] = ()) -> str:
    """Claude's newest text in the transcript, one line, at most LINE_CHARS."""
    turns, _ = transcript_turns(path, limit=TURNS_SHOWN, tail=LINE_TAIL_BYTES, chars=LINE_CHARS * 2,
                                grow_to=LINE_TAIL_BYTES, eve=eve)
    for turn in reversed(turns):
        if turn["role"] == "claude":
            return flat(turn["text"], LINE_CHARS)
    return ""


def _mtime(path: Optional[Path]) -> float:
    if path is None:
        return 0.0
    try:
        return path.stat().st_mtime
    except OSError:
        return 0.0


# ---- the processes -------------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Proc:
    ppid: int
    tty: str                                      # "/dev/ttys003"; "" when it has no terminal
    args: str


def process_table(run: Callable[..., Any] = subprocess.run) -> Optional[dict[int, Proc]]:
    """Every process: pid → its parent, its terminal and its command line, from one ``ps``. None when ps fails."""
    try:
        ps = run([claude_live.PS, "-axww", "-o", "pid=,ppid=,tty=,args="], capture_output=True, text=True,
                 timeout=claude_live.PROBE_TIMEOUT_SECS)
    except (OSError, subprocess.SubprocessError) as e:
        log.debug("work sessions: ps failed (%s)", type(e).__name__)
        return None
    if ps.returncode != 0:
        return None
    out: dict[int, Proc] = {}
    for line in str(ps.stdout).splitlines():
        parts = line.split(None, 3)
        if len(parts) < 4 or not parts[0].isdigit() or not parts[1].isdigit():
            continue
        tty = parts[2]
        out[int(parts[0])] = Proc(int(parts[1]), f"/dev/{tty}" if tty.startswith("tty") else "", parts[3].strip())
    return out or None


def named_session(args: str) -> str:
    """The session a claude command line runs: ``--session-id <id>`` (a new session, or a fork's own id), else
    ``--resume <id>`` / ``-r <id>`` (that session itself, reopened). Only the words before ``--`` count: what follows
    is the first prompt, never a flag, so a task cannot name another session. "" when it names none."""
    words = args.split()
    new = resumed = ""
    fork = False
    for i, word in enumerate(words):
        if word == "--":
            break
        flag, eq, value = word.partition("=")
        if not eq:
            value = words[i + 1] if i + 1 < len(words) else ""
        if flag == "--session-id" and not new:
            new = value
        elif flag in ("--resume", "-r") and not resumed:
            resumed = value
        elif word == "--fork-session":
            fork = True
    if UUID_RE.match(new):
        return new
    return resumed if not fork and UUID_RE.match(resumed) else ""


def _claude_like(args: str) -> bool:
    """A Claude Code process as claude_live tells one, or one run straight from its install's versions folder
    (``~/.local/share/claude/versions/<version>``). The completeness check counts these: a session whose process
    it missed would read as ended, and only "cannot tell" is safe there."""
    words = args.split()
    return claude_live._is_claude(args) or bool(words and "/claude/versions/" in words[0])


def _flags(args: str) -> set[str]:
    """The option names on a command line, read only before ``--``: a Work session's task follows ``--``, and a task
    like "mkdir -p src" is never a flag."""
    words = args.split()
    return {w.partition("=")[0] for w in (words[: words.index("--")] if "--" in words else words)}


ONE_SHOT_FLAGS = frozenset({"-p", "--print"})            # a one-shot run: a hook's drafter, a script
NEVER_A_SESSION = frozenset({"--chrome-native-host"})    # Chrome's native messaging host, never in the registry


def _interactive_claude(args: str) -> bool:
    """A Claude Code process someone types into: none of claude_live's headless flags (``-p``, ``--print``...) before
    ``--``. A task that reads like one must not make its own session look headless: ps would lose it, and stop would
    signal nothing."""
    return _claude_like(args) and not claude_live.HEADLESS_FLAGS & _flags(args)


def claude_sessions(table: dict[int, Proc]) -> dict[str, int]:
    """Each running interactive Claude Code process whose command line names its session (``named_session``) → its
    pid. A headless ``claude -p <prompt>`` (a hook's drafter, an SDK worker) is never one: its prompt comes before any
    ``--``, so its words could pass for flags."""
    out: dict[str, int] = {}
    for pid, proc in table.items():
        if _interactive_claude(proc.args):
            sid = named_session(proc.args)
            if sid:
                out[sid] = pid
    return out


def session_pids(run: Callable[..., Any] = subprocess.run) -> Optional[dict[str, int]]:
    """Each running Claude Code process started with ``--session-id <uuid>`` or ``--resume <uuid>`` → its pid; None
    when ps fails."""
    table = process_table(run)
    return None if table is None else claude_sessions(table)


_APP = re.compile(r"/([^/]+?)\.app/Contents/")
APP_NAMES = {"Visual Studio Code": "VS Code", "Code": "VS Code", "iTerm2": "iTerm"}
HOSTS = {"tmux": "tmux", "screen": "screen", "sshd": "SSH", "mosh-server": "mosh"}    # not apps, but where it runs


def where(pid: Optional[int], table: Optional[dict[int, Proc]]) -> str:
    """The app a process runs in: the first ``<Name>.app`` on its ancestors' command lines (Warp, Terminal, iTerm,
    VS Code, Cursor...), or tmux, screen or SSH when one of those comes first. "" when none says."""
    if pid is None or table is None:
        return ""
    seen: set[int] = set()
    proc = table.get(pid)
    while proc is not None and proc.ppid > 1 and proc.ppid not in seen and len(seen) < MAX_ANCESTORS:
        seen.add(proc.ppid)
        proc = table.get(proc.ppid)
        if proc is None:
            break
        found = _APP.search(proc.args)
        if found:
            return APP_NAMES.get(found.group(1), found.group(1))
        first = proc.args.split()[0] if proc.args.split() else ""
        host = HOSTS.get(os.path.basename(first).split(":")[0])
        if host:
            return host
    return ""


class Seen:
    """One look at the Mac, taken on a worker thread: ps's process table and Claude Code's session registry."""

    def __init__(self, procs: Optional[dict[int, Proc]], peers: Optional[dict[str, Any]]) -> None:
        self.procs = procs
        self.peers = peers
        self.pids = claude_sessions(procs) if procs is not None else None
        self._by_pid = {getattr(p, "pid", None): p for p in (peers or {}).values()}

    def pid(self, sid: str) -> Optional[int]:
        return self.pids.get(sid) if self.pids is not None else None

    def peer(self, sid: str, pid: Optional[int] = None) -> Any:
        """The session's entry in Claude Code's registry: by its id, else (a /clear moved it to a new id) by the pid
        ps gives for it."""
        if not self.peers:
            return None
        got = self.peers.get(sid)
        if got is None and pid is not None:
            got = self._by_pid.get(pid)
        return got

    def complete(self) -> bool:
        """Every claude process ps shows is in Claude Code's registry (socket or not), so a session the registry does
        not list has ended. Headless ones count too: an IDE's session runs claude with ``--output-format`` and
        ``--input-format`` (the SDK's way), and a ``claude -p`` registers like any session. Only Chrome's native host,
        never a session, is left out. False when either look failed, or when any claude is missing from it (an older
        Claude Code, which never registers)."""
        if self.procs is None or self.peers is None:
            return False
        for pid, proc in self.procs.items():
            if _claude_like(proc.args) and not NEVER_A_SESSION & _flags(proc.args) and pid not in self._by_pid:
                return False
        return True

    def one_shot(self, pid: Optional[int]) -> bool:
        """ps shows this process as a one-shot ``claude -p`` run (a hook's drafter, a script). It registers like a
        session, but nobody talks to it, so the list leaves it out."""
        proc = self.procs.get(pid) if self.procs is not None and pid is not None else None
        return proc is not None and _claude_like(proc.args) and bool(ONE_SHOT_FLAGS & _flags(proc.args))

    def tty(self, pid: Optional[int]) -> str:
        proc = self.procs.get(pid) if self.procs is not None and pid is not None else None
        return proc.tty if proc is not None else ""

    def where(self, pid: Optional[int]) -> str:
        return where(pid, self.procs)


def launch_argv(sid: str, task: str, mcp_config: Path, claude: str = "claude", resume: bool = False) -> list[str]:
    """Claude Code's argv for a Work session. ``--mcp-config`` takes several values, so the session flag follows it,
    and ``--`` ends the options: the task is the one positional argument (the first prompt), even when it starts with
    a dash. ``resume``: reopen session ``sid`` itself (``--resume <sid>``, never ``--fork-session``), so it keeps its
    id and its transcript; else start a new one under that id (``--session-id``)."""
    if not UUID_RE.match(sid):
        raise ValueError("not a session id")
    which = ["--resume", sid] if resume else ["--session-id", sid]
    return [claude, "--dangerously-skip-permissions", "--mcp-config", str(mcp_config), *which, "--", task]


def mcp_config(sid: str, token: str, python: str, environ: Any = None) -> dict[str, Any]:
    """eve's MCP server for one session (eve_mcp.py): it speaks to the running daemon with this session's token."""
    env = os.environ if environ is None else environ
    server_env = {"EVE_MCP_SESSION": sid, "EVE_MCP_TOKEN": token}
    if env.get("EVE_BRIDGE_SOCK"):
        server_env["EVE_BRIDGE_SOCK"] = str(env["EVE_BRIDGE_SOCK"])
    return {"mcpServers": {"eve": {"type": "stdio", "command": python, "args": ["-P", "-m", "eve_bridge.eve_mcp"],
                                   "env": server_env}}}


def _write_private(path: Path, text: str) -> None:
    path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    with contextlib.suppress(OSError):
        path.parent.chmod(0o700)
    fd = os.open(str(path), os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        f.write(text)
    os.chmod(path, 0o600)


async def _osascript(script: str, *args: str) -> tuple[int, str]:
    """osascript with ``script`` as its source and ``args`` as ``argv`` → (exit code, its stdout or stderr)."""
    proc = await asyncio.create_subprocess_exec("osascript", "-e", script, *args, stdout=asyncio.subprocess.PIPE,
                                                stderr=asyncio.subprocess.PIPE)
    out, err = await proc.communicate()
    code = proc.returncode or 0
    return code, (out if code == 0 else err).decode(errors="replace").strip()[:200]


def _hash(token: str) -> str:
    return hashlib.sha256(token.encode()).hexdigest()


def _peer_state(status: Any) -> str:
    """A registry status as a row's state: "busy" is working; one that waits on its user is waiting; else idle."""
    s = str(status or "").lower()
    if s == "busy":
        return "working"
    if any(word in s for word in ("wait", "input", "permission", "approv")):
        return "waiting"
    return "idle"


def _read_peers() -> Optional[dict[str, Any]]:
    """Every live interactive session in Claude Code's registry, with or without a messaging socket
    (claude_peer.live_sessions): liveness wants them all, and only those with a socket take messages."""
    return claude_peer.live_sessions()


def messageable(peer: Any) -> bool:
    """A registry entry eve can send to: it has a socket, and speaks a protocol claude_peer speaks."""
    return (peer is not None and bool(getattr(peer, "socket_path", ""))
            and getattr(peer, "protocol", 0) in getattr(claude_peer, "PROTOCOLS", (1,)))


# ---- the registry --------------------------------------------------------------------------------------------------

SAVED_FIELDS = ("sid", "created_at", "title", "token_hash", "client_id", "window_id", "tty", "transcript_path",
                "stopped", "ended_at", "cwd", "resumed_from")


@dataclass
class Record:
    sid: str
    created_at: float                             # when eve last opened it (started, or reopened)
    title: str
    token_hash: str
    client_id: str = ""
    window_id: Optional[int] = None
    tty: str = ""
    transcript_path: str = ""
    stopped: bool = False
    ended_at: float = 0.0
    cwd: str = ""                                 # its folder; "" is the work folder
    resumed_from: str = ""                        # the session it reopens (its own id; an older fork's: another's)
    phase: str = "starting"                       # not saved: starting, working, waiting, idle
    last_activity: float = 0.0                    # not saved: the newest hook

    @property
    def over(self) -> bool:
        return self.stopped or self.ended_at > 0

    def saved(self) -> dict[str, Any]:
        d = asdict(self)
        return {k: d[k] for k in SAVED_FIELDS}


class WorkSessions:
    """The Work sessions eve started or reopened, every other live Claude Code session, and the routes' logic.
    Everything that touches the Mac is injectable: ``osascript`` (script, *argv) → (code, out), ``run`` for ps,
    ``kill`` for signals, ``live`` for the claude processes' folders (claude_live.live_processes), ``peers`` for
    Claude Code's session registry (claude_peer.live_sessions: every live interactive session by id, with or without
    a socket; None or an exception is "cannot tell") and ``send`` (peer, text) → claude_peer.Delivery for a message
    into one (claude_peer.send). ``eve_addresses``: where eve's inbox listens (the daemon adds it), so a session's
    answer to eve shows in its sheet."""

    def __init__(self, *, root: Optional[Path] = None, home: Optional[Path] = None,
                 state: Callable[[], Any] = lambda: None, osascript: Callable[..., Any] = _osascript,
                 run: Callable[..., Any] = subprocess.run, kill: Callable[[int, int], None] = os.kill,
                 live: Optional[Callable[[], Optional[dict[str, bool]]]] = None,
                 peers: Callable[[], Optional[dict[str, Any]]] = _read_peers,
                 send: Callable[..., Awaitable[Any]] = claude_peer.send,
                 clock: Callable[[], float] = time.time, python: str = sys.executable,
                 sleep: Callable[[float], Any] = asyncio.sleep, environ: Any = None,
                 claude_dir: Optional[Path] = None) -> None:
        self.root = Path(root) if root is not None else work_root(environ)
        self.home = Path(home) if home is not None else CONFIG_DIR
        self.registry = self.home / REGISTRY_NAME
        self.files = files_dir(self.home)
        self._state = state
        self._osascript = osascript
        self._run = run
        self._kill = kill
        self._live = live if live is not None else (lambda: claude_live.live_processes(run))
        self._peers = peers
        self._send = send
        self._clock = clock
        self.python = python
        self._sleep = sleep
        self._environ = environ
        self._claude_dir = claude_dir
        self._lock = asyncio.Lock()                         # opening a window: one at a time
        self._order: dict[str, tuple[asyncio.Lock, int]] = {}   # sends to one session go out in the order they came
        self._continued: dict[str, tuple[float, int, dict[str, Any]]] = {}   # client_id -> a delivered continue
        self.eve_addresses: set[str] = set()
        self.records: dict[str, Record] = {}
        self._load()

    # -- the file --
    def _load(self) -> None:
        try:
            got = json.loads(self.registry.read_text(encoding="utf-8"))
        except FileNotFoundError:
            return
        except (OSError, ValueError) as e:
            log.warning("work sessions: the registry could not be read (%s); starting empty", type(e).__name__)
            return
        rows = got.get("sessions", []) if isinstance(got, dict) else []
        for row in rows:
            if not isinstance(row, dict) or not UUID_RE.match(str(row.get("sid", ""))):
                continue
            try:
                rec = Record(sid=row["sid"], created_at=float(row.get("created_at") or 0),
                             title=str(row.get("title") or ""), token_hash=str(row.get("token_hash") or ""),
                             client_id=str(row.get("client_id") or ""),
                             window_id=int(row["window_id"]) if row.get("window_id") is not None else None,
                             tty=str(row.get("tty") or ""), transcript_path=str(row.get("transcript_path") or ""),
                             stopped=bool(row.get("stopped")), ended_at=float(row.get("ended_at") or 0),
                             cwd=str(row.get("cwd") or ""), resumed_from=str(row.get("resumed_from") or ""))
            except (TypeError, ValueError):
                continue
            rec.phase = "idle"
            rec.last_activity = rec.created_at
            self.records[rec.sid] = rec

    def _save(self) -> None:
        now = self._clock()
        self.records = {sid: r for sid, r in self.records.items()
                        if not (r.over and now - max(r.ended_at, r.created_at) > KEEP_SECS)}
        try:
            tmp = self.registry.with_suffix(".tmp")
            _write_private(tmp, json.dumps({"sessions": [r.saved() for r in self.records.values()]}, indent=1))
            tmp.replace(self.registry)
        except OSError as e:
            log.warning("work sessions: the registry could not be saved (%s)", type(e).__name__)

    # -- the hooks (the daemon calls this from its IPC handlers) --
    def note(self, evt: str, req: dict[str, Any]) -> None:
        """A hook event for any session; only eve's own sessions are recorded. Never raises."""
        sid = req.get("session_id")
        rec = self.records.get(sid) if isinstance(sid, str) else None
        if rec is None or rec.over:
            return
        rec.last_activity = self._clock()
        if evt == "session_start":
            path = req.get("transcript_path")
            if isinstance(path, str) and path and path != rec.transcript_path:
                rec.transcript_path = path
                self._save()
            if rec.phase == "starting":
                rec.phase = "idle"
        elif evt == "turn_begin":
            rec.phase = "working"
        elif evt == "turn_end":
            rec.phase = "idle"
        elif evt == "notification":
            kind = req.get("notification_type")
            if notification_waits(kind if isinstance(kind, str) else None):
                rec.phase = "waiting"
        elif evt == "pretooluse":
            if req.get("tool_name") == "AskUserQuestion":
                rec.phase = "waiting"
        elif evt == "posttooluse":
            if rec.phase == "waiting":
                rec.phase = "working"
        elif evt == "session_end":
            # Not the end of the process: /clear ends this session id and goes on under a new one, in the same
            # claude process (whose ps line still names this id). Whether it runs is ps's to say (list, reply).
            rec.phase = "idle"

    def heard(self, from_address: str, from_name: str, text: str) -> None:
        """A message to eve's inbox (claude_peer.Inbox's on_message): a session answering the owner. eve never acts on
        one; the session's sheet shows it, from that session's own transcript. Logged by size only."""
        log.info("work sessions: a session answered eve (%d characters); its sheet shows it", len(text or ""))

    def receipt(self, msg_id: str, state: str, detail: str) -> None:
        """A later word about a message that was held (claude_peer.Inbox's on_receipt): approved, declined or expired on
        the Mac. Logged only; an approved message lands in the session's transcript, so its sheet shows it."""
        log.info("work sessions: a held message to a session was answered on the Mac (%s)", flat(state, 20))

    def check_token(self, sid: Any, token: Any) -> bool:
        """Whether ``token`` is the eve MCP token of a Work session that has not ended (constant time)."""
        rec = self.records.get(sid) if isinstance(sid, str) else None
        if rec is None or rec.over or not isinstance(token, str) or not token or not rec.token_hash:
            return False
        return hmac.compare_digest(_hash(token), rec.token_hash)

    # -- helpers --
    def _folder(self, rec: Record) -> Path:
        return Path(rec.cwd) if rec.cwd else self.root

    def _session(self, sid: str) -> Any:
        state = self._state()
        return getattr(state, "sessions", {}).get(sid) if state is not None else None

    def _transcript(self, rec: Optional[Record], session: Any = None, sid: str = "", peer: Any = None) -> Optional[Path]:
        if rec is not None and rec.transcript_path:
            return Path(rec.transcript_path)
        path = getattr(session, "transcript_path", None)
        if isinstance(path, str) and path:
            return Path(path)
        if rec is not None:
            return derived_transcript(self._folder(rec), rec.sid, self._claude_dir)
        cwd = getattr(session, "cwd", None) or getattr(peer, "cwd", None)
        return found_transcript(cwd, getattr(session, "session_id", "") or sid, self._claude_dir)

    def _see(self) -> Seen:
        """ps and Claude Code's registry, read together (on a worker thread). A registry that cannot be read is
        "cannot tell" (None), never "no sessions"."""
        try:
            peers = self._peers()
        except Exception as e:  # noqa: BLE001 — claude_peer reads files other programs write; any failure is a blind look
            log.debug("work sessions: the session registry could not be read (%s)", type(e).__name__)
            peers = None
        if peers is not None and not isinstance(peers, dict):
            peers = None
        return Seen(process_table(self._run), peers)

    def _end(self, rec: Record) -> None:
        if not rec.over:
            rec.ended_at = self._clock()
            self._save()
        self._forget_files(rec.sid)
        with contextlib.suppress(OSError):
            (self.files / f"{rec.sid}.resume").unlink()       # left by a fork made before 2026-10-26

    def _alive(self, sid: str, rec: Optional[Record], session: Any, seen: Seen,
               folders: Callable[[], Optional[dict[str, bool]]], cwd: str = "") -> Optional[bool]:
        """Whether a session runs: True, False (it has ended), or None (eve cannot tell). ``folders``: the claude
        processes' folders, asked only when the registry is not complete (claude_live's older test). An eve session
        that ended is judged like any other: the owner can reopen it at the Mac (``claude --resume``), and reopening
        it again from the phone would make two agents of one. Blocking."""
        now = self._clock()
        if rec is not None and not rec.over:
            if seen.pid(sid) is not None or seen.peer(sid) is not None or now - rec.created_at < GRACE_SECS:
                return True
            return False if seen.pids is not None else None
        if seen.peer(sid) is not None or seen.pid(sid) is not None:
            return True
        if session is not None and now - getattr(session, "started_at", 0.0) < GRACE_SECS:
            return True
        if seen.complete():
            return False
        live = folders()
        if live is None:
            return None
        cwd = cwd or (str(self._folder(rec)) if rec is not None else "") or str(getattr(session, "cwd", "") or "")
        if cwd and claude_live.alive(cwd, set(live)):
            return None                                   # a claude runs in its folder: maybe this session
        return False

    def _row(self, sid: str, seen: Seen, alive: Optional[bool], *, rec: Optional[Record] = None, session: Any = None,
             peer: Any = None, path: Optional[Path] = None, line: Optional[str] = None) -> dict[str, Any]:
        """One session as the phone sees it. Reads the transcript (its last line), so it runs on a worker thread."""
        if rec is not None and rec.over and alive is True:
            rec = None                                   # eve's session ended, and was reopened at the Mac since
        pid = seen.pid(sid)
        peer = peer if peer is not None else seen.peer(sid, pid)
        if pid is None and peer is not None:
            pid = getattr(peer, "pid", None)
        if path is None:
            path = self._transcript(rec, session, sid, peer)
        if line is None:
            line = last_line(path, self.eve_addresses)
        now = self._clock()
        if rec is not None:
            opening = pid is None and peer is None and now - rec.created_at < GRACE_SECS
            state = "starting" if opening and seen.pids is not None else rec.phase if rec.phase in STATES else "idle"
            if state == "starting" and (pid is not None or peer is not None):
                # claude is up, but no SessionStart hook said so (one that missed the daemon): the registry's word
                state = _peer_state(getattr(peer, "status", "")) if peer is not None else "idle"
            typed = bool(seen.tty(pid)) or opening       # an eve session in Terminal can be typed into
            return {"id": sid, "folder": self._folder(rec).name, "title": rec.title, "state": state,
                    "where": seen.where(pid) or "Terminal",
                    "last_activity": int(max(rec.last_activity, rec.created_at, _mtime(path))), "last_line": line,
                    "from_work": True,
                    "can_reply": alive is not False and (messageable(peer) or (alive is True and typed)),
                    "can_stop": not rec.over, "can_continue": False}
        folder = Path(transcript_cwd(path) or getattr(session, "cwd", None) or getattr(peer, "cwd", "") or "").name
        if session is not None:
            hooks = self._state()
            waiting = bool(hooks is not None and hooks._needs_input_live(session))
            state = "waiting" if waiting else "working" if session.running else "idle"
        else:
            state = _peer_state(getattr(peer, "status", ""))
        name = getattr(peer, "name", "") if peer is not None else ""
        return {"id": sid, "folder": folder, "title": str(name or folder), "state": state, "where": seen.where(pid),
                "last_activity": int(max(getattr(session, "started_at", 0.0), _mtime(path))), "last_line": line,
                "from_work": False, "can_reply": alive is True and messageable(peer), "can_stop": False,
                "can_continue": False}

    def _opened_row(self, rec: Record) -> dict[str, Any]:
        """The row of a session eve has just opened: starting, in Terminal, taking replies (they wait for it)."""
        return {"id": rec.sid, "folder": self._folder(rec).name, "title": rec.title, "state": "starting",
                "where": "Terminal", "last_activity": int(max(rec.last_activity, rec.created_at)), "last_line": "",
                "from_work": True, "can_reply": True, "can_stop": True, "can_continue": False}

    @staticmethod
    def _continuable(cwd: Any, path: Optional[Path]) -> bool:
        """A session that can be reopened: its folder is there and so is its transcript."""
        return bool(cwd) and Path(str(cwd)).is_dir() and path is not None and path.is_file()

    def _resumable(self, cwd: str, path: Optional[Path]) -> bool:
        return self._continuable(cwd, path) and folder_trusted(cwd, self._claude_dir) is not False

    def _home_of(self, rec: Optional[Record], session: Any, path: Optional[Path]) -> str:
        """The folder a session runs in: eve's own record's, else where its transcript says it started, else (that
        folder is gone) where its shell last was. Blocking (reads the transcript's head)."""
        if rec is not None:
            return str(self._folder(rec))
        started = transcript_cwd(path)
        return started if started and Path(started).is_dir() else str(getattr(session, "cwd", "") or "")

    def _rows(self, records: list[Record], sessions: dict[str, Any], seen: Seen) -> tuple[list[dict[str, Any]],
                                                                                           list[Record]]:
        """(the rows, eve's records whose process is gone). Runs on a thread (it reads transcripts), so it gets
        snapshots of the records and the daemon's sessions, taken on the loop."""
        rows: list[dict[str, Any]] = []
        gone: list[Record] = []
        listed: set[str] = set()
        folders = _once(self._live)
        for rec in records:
            if rec.over:
                continue
            alive = self._alive(rec.sid, rec, None, seen, folders)
            if alive is False:
                gone.append(rec)
                continue
            rows.append(self._row(rec.sid, seen, alive, rec=rec))
            listed.add(rec.sid)
        for sid, s in sessions.items():
            if sid in listed or not getattr(s, "cwd", None):
                continue
            alive = self._alive(sid, None, s, seen, folders)
            if alive is False or seen.one_shot(seen.pid(sid) or getattr(seen.peer(sid), "pid", None)):
                continue
            rows.append(self._row(sid, seen, alive, session=s))
            listed.add(sid)
        for sid, peer in (seen.peers or {}).items():
            if sid in listed or not UUID_RE.match(str(sid)) or seen.one_shot(getattr(peer, "pid", None)):
                continue
            rows.append(self._row(sid, seen, True, peer=peer))
            listed.add(sid)
        rows.sort(key=lambda r: r["last_activity"], reverse=True)
        return rows[:MAX_LISTED], gone

    @contextlib.asynccontextmanager
    async def _in_order(self, sid: str) -> AsyncIterator[None]:
        """Sends to one session go out one at a time, in the order they came (a lock per session, never a refusal);
        other sessions are not held up."""
        lock, users = self._order.get(sid, (asyncio.Lock(), 0))
        self._order[sid] = (lock, users + 1)
        try:
            async with lock:
                yield
        finally:
            lock, users = self._order[sid]
            if users <= 1:
                del self._order[sid]
            else:
                self._order[sid] = (lock, users - 1)

    # -- the routes: each answers (HTTP status, JSON body) --
    async def list(self) -> tuple[int, dict[str, Any]]:
        state = self._state()
        sessions = dict(getattr(state, "sessions", {})) if state is not None else {}
        seen = await asyncio.to_thread(self._see)
        opened = {sid: rec.created_at for sid, rec in self.records.items()}
        rows, gone = await asyncio.to_thread(self._rows, list(self.records.values()), sessions, seen)
        for rec in gone:
            self._gone(rec, opened.get(rec.sid))
        return 200, {"ok": True, "folder": self.root.name, "sessions": rows}

    def _gone(self, rec: Record, opened: Optional[float]) -> None:
        """End a record a look on a worker thread found without a process, unless it was reopened meanwhile (a
        continue that resumed it while the look ran): ending that one would delete the task its window is about to
        read."""
        if rec.over or rec.created_at != opened:
            return
        log.info("work sessions: %s is gone (no claude process)", rec.sid[:8])
        self._end(rec)

    async def start(self, task: Any, client_id: Any = "") -> tuple[int, dict[str, Any]]:
        if not isinstance(task, str) or not task.strip():
            return 400, {"ok": False, "reason": "say what Claude should work on"}
        task = task.strip()
        if len(task) > MAX_TASK_CHARS:
            return 400, {"ok": False, "reason": f"at most {MAX_TASK_CHARS} characters"}
        if client_id is None:
            client_id = ""
        if not isinstance(client_id, str) or len(client_id) > MAX_CLIENT_ID_CHARS:
            return 400, {"ok": False, "reason": "client_id must be a string of at most 64 characters"}
        async with self._lock:
            now = self._clock()
            if client_id:
                for rec in self.records.values():
                    if rec.client_id == client_id and now - rec.created_at < DEDUPE_SECS:
                        return 200, {"ok": True, "session": self._opened_row(rec)}
            try:
                self.root.mkdir(parents=True, exist_ok=True)
            except OSError as e:
                return 503, {"ok": False, "reason": f"the work folder is not there ({type(e).__name__})"}
            rec, problem = await self._open(task, client_id, now)
            if rec is None:
                return 503, {"ok": False, "reason": problem}
            log.info("work sessions: %s started (task %d characters)", rec.sid[:8], len(task))
            return 200, {"ok": True, "session": self._opened_row(rec)}

    async def _open(self, task: str, client_id: str, now: float, *, cwd: str = "", sid: str = "", title: str = "",
                    transcript: str = "") -> tuple[Optional[Record], str]:
        """Write a session's files and open its Terminal window: (the record, "") or (None, why not). The caller holds
        the lock. ``sid``: an ended session to reopen as itself (``claude --resume``), its record kept or made under
        that id; "" mints a new one. ``cwd``: its folder ("" is the work folder)."""
        resume = bool(sid)
        if resume and not UUID_RE.match(sid):
            return None, UNKNOWN                         # the id goes into a shell line: only a checked UUID does
        sid = sid or str(uuid.uuid4())
        token = secrets.token_urlsafe(32)
        rec = self.records.get(sid) if resume else None
        before = rec.saved() if rec is not None else None
        if rec is None:
            rec = Record(sid=sid, created_at=now, title=title or flat(task.splitlines()[0], TITLE_CHARS),
                         token_hash=_hash(token), client_id=client_id, cwd=cwd, transcript_path=transcript,
                         resumed_from=sid if resume else "")
        else:                                       # eve's own session, ended: the same record, open again
            rec.created_at, rec.token_hash, rec.client_id = now, _hash(token), client_id
            rec.stopped, rec.ended_at, rec.window_id, rec.tty, rec.resumed_from = False, 0.0, None, "", sid
        rec.phase = "starting"
        rec.last_activity = now
        try:
            _write_private(self.files / f"{sid}.task", task)
            _write_private(self.files / f"{sid}.mcp.json",
                           json.dumps(mcp_config(sid, token, self.python, self._environ)))
        except OSError as e:
            log.warning("work sessions: could not write the session's files (%s)", type(e).__name__)
            self._forget_files(sid)
            self._undo(rec, before)
            return None, "eve couldn't write the session's files"
        self.records[sid] = rec
        self._save()
        # Only code-made words: the folder (quoted), this Python (quoted), the module, a verb and a checked UUID.
        # -P: Python leaves the cwd (the folder, where sessions write files) off sys.path, so a json.py or
        # uuid.py a session left there cannot shadow the stdlib or run in this interpreter.
        line = (f"cd {shlex.quote(str(self._folder(rec)))} && exec {shlex.quote(self.python)} "
                f"-P -m eve_bridge.work_sessions {'resume' if resume else 'launch'} {sid}")
        try:
            code, out = await self._osascript(TERMINAL_OPEN, line)
        except OSError as e:
            code, out = 1, type(e).__name__
        if code != 0:
            log.warning("work sessions: Terminal did not open (exit %d)", code)
            self._forget_files(sid)
            self._undo(rec, before)
            self._save()
            return None, "eve couldn't open Terminal on the Mac"
        wid, _, tty = str(out).strip().partition(" ")
        # the window opened; without its id or tty a reply still finds its tab by the claude process's tty
        rec.window_id = int(wid) if wid.isdigit() and int(wid) > 0 else None
        rec.tty = tty.strip() if tty.strip().startswith("/dev/") else ""
        self._save()
        return rec, ""

    def _forget_files(self, sid: str) -> None:
        for name in (f"{sid}.task", f"{sid}.mcp.json"):
            with contextlib.suppress(OSError):
                (self.files / name).unlink()

    def _undo(self, rec: Record, before: Optional[dict[str, Any]]) -> None:
        """A session that did not open: a new record goes, a reopened one is put back as it was (ended)."""
        if before is None:
            self.records.pop(rec.sid, None)
            return
        for key, value in before.items():
            setattr(rec, key, value)
        rec.phase = "idle"

    async def continue_session(self, sid: str, text: Any, client_id: Any = "") -> tuple[int, dict[str, Any]]:
        """The owner's words to a session from its sheet. A live one (any terminal, any app) takes them as a reply
        (``reply``'s answer, plus ``session``): it is never forked and no window opens. One that has ended reopens as
        itself (``claude --resume <sid>`` in its own folder, ``text`` its first prompt) in a new Terminal window and
        answers ``delivered: "resumed"``; eve keeps a record under the same id. 409 codes beside the reply's: "unsure"
        (eve cannot tell whether it runs), "no_resume" (no folder or transcript), "untrusted" (the folder's trust prompt
        was never accepted)."""
        if not isinstance(text, str) or not text.strip():
            return 400, {"ok": False, "reason": "say what Claude should do next"}
        text = text.strip()
        if len(text) > MAX_TASK_CHARS:
            return 400, {"ok": False, "reason": f"at most {MAX_TASK_CHARS} characters"}
        if client_id is None:
            client_id = ""
        if not isinstance(client_id, str) or len(client_id) > MAX_CLIENT_ID_CHARS:
            return 400, {"ok": False, "reason": "client_id must be a string of at most 64 characters"}
        async with self._in_order(sid):
            again = self._again(client_id)
            if again is not None:
                return again
            rec = self.records.get(sid)
            session = self._session(sid)
            seen = await asyncio.to_thread(self._see)
            peer = seen.peer(sid, seen.pid(sid))
            path = await asyncio.to_thread(self._transcript, rec, session, sid, peer)
            if rec is None and session is None and peer is None and (path is None or not path.is_file()):
                return 404, {"ok": False, "reason": UNKNOWN}
            alive = await asyncio.to_thread(self._alive, sid, rec, session, seen, _once(self._live),
                                            await asyncio.to_thread(transcript_cwd, path))
            if alive is True:
                status, out = await self._deliver(sid, text, seen)
                if status == 200:
                    out["session"] = await asyncio.to_thread(self._row, sid, seen, True, rec=self.records.get(sid),
                                                             session=self._session(sid))
                    if client_id:
                        self._continued[client_id] = (self._clock(), status, out)
                return status, out
            if alive is None:
                return 409, {"ok": False, "reason": UNSURE, "code": "unsure"}
            if rec is not None and not rec.over:
                log.info("work sessions: %s is gone (no claude process)", sid[:8])
                self._end(rec)
            cwd = await asyncio.to_thread(self._home_of, rec, session, path)
            if not await asyncio.to_thread(self._continuable, cwd, path):
                return 409, {"ok": False, "reason": NO_CONTINUE, "code": "no_resume"}
            if await asyncio.to_thread(folder_trusted, cwd, self._claude_dir) is False:
                return 409, {"ok": False, "reason": UNTRUSTED, "code": "untrusted"}
            title = rec.title if rec is not None and rec.title else Path(cwd).name
            async with self._lock:
                opened, problem = await self._open(text, client_id, self._clock(), cwd=cwd, sid=sid, title=title,
                                                   transcript=str(path) if path is not None else "")
            if opened is None:
                return 503, {"ok": False, "reason": problem}
        log.info("work sessions: %s reopened as itself (%d characters)", sid[:8], len(text))
        return 200, {"ok": True, "delivered": "resumed", "session": self._opened_row(opened)}

    def _again(self, client_id: str) -> Optional[tuple[int, dict[str, Any]]]:
        """A retry of a send with the same client_id within DEDUPE_SECS: the first one's answer, sent once."""
        if not client_id:
            return None
        now = self._clock()
        self._continued = {k: v for k, v in self._continued.items() if now - v[0] < DEDUPE_SECS}
        done = self._continued.get(client_id)
        if done is not None:
            return done[1], dict(done[2])
        for rec in self.records.values():                # a reopen it already made
            if rec.client_id == client_id and now - rec.created_at < DEDUPE_SECS and not rec.over:
                return 200, {"ok": True, "delivered": "resumed", "session": self._opened_row(rec)}
        return None

    async def detail(self, sid: str) -> tuple[int, dict[str, Any]]:
        rec = self.records.get(sid)
        opened = rec.created_at if rec is not None else None
        session = self._session(sid)
        seen = await asyncio.to_thread(self._see)
        peer = seen.peer(sid, seen.pid(sid))
        path = await asyncio.to_thread(self._transcript, rec, session, sid, peer)
        if rec is None and session is None and peer is None and (path is None or not path.is_file()):
            return 404, {"ok": False, "reason": UNKNOWN}
        started = await asyncio.to_thread(transcript_cwd, path)
        alive = await asyncio.to_thread(self._alive, sid, rec, session, seen, _once(self._live), started)
        if rec is not None and alive is False:
            self._gone(rec, opened)                      # claude exited (/exit, a crash): the sheet shows it ended
        turns, cut = await asyncio.to_thread(transcript_turns, path, eve=tuple(self.eve_addresses))
        row = await asyncio.to_thread(self._row, sid, seen, alive, rec=rec, session=session, peer=peer, path=path,
                                      line="")
        ended = alive is False and (rec is None or rec.over)     # not one a continue reopened meanwhile
        if ended:
            cwd = await asyncio.to_thread(self._home_of, rec, session, path)
            row.update(state="ended", can_reply=False, can_stop=False,
                       can_continue=await asyncio.to_thread(self._resumable, cwd, path))
        last = next((t["text"] for t in reversed(turns) if t["role"] == "claude"), "")
        row["last_line"] = flat(last, LINE_CHARS)
        return 200, {"ok": True, "session": row, "turns": turns, "cut": cut, "ended": ended}

    async def reply(self, sid: str, text: Any) -> tuple[int, dict[str, Any]]:
        """The owner's words to a live session, whatever it runs in (``_deliver``). 200 ``{"ok": true, "delivered":
        "now" | "queued" | "held" | "typed"}`` (``reason`` beside "held"); 409 ``{"ok": false, "reason", "code"}``."""
        if not isinstance(text, str) or not text.strip():
            return 400, {"ok": False, "reason": "nothing to send"}
        text = text.strip()
        if len(text) > MAX_REPLY_CHARS:
            return 400, {"ok": False, "reason": f"at most {MAX_REPLY_CHARS} characters"}
        async with self._in_order(sid):
            return await self._deliver(sid, text)

    async def _deliver(self, sid: str, text: str, seen: Optional[Seen] = None) -> tuple[int, dict[str, Any]]:
        """Into the session's messaging socket (claude_peer.send); only an eve session in Terminal with no usable socket
        is typed into instead (``_type``). An eve session still opening is waited for, up to STARTING_WAIT_SECS, so a
        send right after start or resume goes through. The caller holds the session's order lock."""
        waited = 0.0
        while True:
            if seen is None:
                seen = await asyncio.to_thread(self._see)
            known = self.records.get(sid)
            rec = known if known is not None and not known.over else None    # an ended one may run again elsewhere
            pid = seen.pid(sid)
            peer = seen.peer(sid, pid)
            if pid is None and peer is not None:
                pid = getattr(peer, "pid", None)
            opening = rec is not None and self._clock() - rec.created_at < GRACE_SECS
            if messageable(peer):
                return await self._send_to(sid, peer, text, rec, seen.tty(pid))
            if opening and waited < STARTING_WAIT_SECS:
                await self._sleep(POLL_SECS)             # claude is not up, or its socket is not yet
                waited += POLL_SECS
                seen = None
                continue
            if rec is not None:
                if pid is not None:
                    return await self._type(rec, seen.tty(pid), text)
                if opening:
                    return 409, {"ok": False, "reason": STARTING, "code": "starting"}
                if seen.pids is None:
                    return 409, {"ok": False, "reason": NO_WINDOW, "code": "no_window"}    # ps failed: never guess
                log.info("work sessions: %s is gone (no claude process)", sid[:8])
                self._end(rec)
                return 409, {"ok": False, "reason": ENDED, "code": ENDED_CODE}
            if peer is not None:                         # running, but with no socket eve can use
                return 409, {"ok": False, "reason": NO_SOCKET, "code": "no_socket"}
            session = self._session(sid)
            path = await asyncio.to_thread(self._transcript, known, session, sid)
            if known is None and session is None and (path is None or not path.is_file()):
                return 404, {"ok": False, "reason": UNKNOWN}
            alive = await asyncio.to_thread(self._alive, sid, known, session, seen, _once(self._live),
                                            await asyncio.to_thread(transcript_cwd, path))
            if alive is False:
                return 409, {"ok": False, "reason": ENDED, "code": ENDED_CODE}
            return 409, {"ok": False, "reason": NO_SOCKET, "code": "no_socket"}

    async def _send_to(self, sid: str, peer: Any, text: str, rec: Optional[Record],
                       tty: str) -> tuple[int, dict[str, Any]]:
        try:
            got = await self._send(peer, text)
        except Exception as e:  # noqa: BLE001 — send never raises by contract; if it does, the words did not go
            log.warning("work sessions: sending to %s failed (%s)", sid[:8], type(e).__name__)
            got = None
        state = str(getattr(got, "state", "error") or "error")
        detail = str(getattr(got, "detail", "") or "").strip()
        if state == "delivered":
            # a busy session takes it at its next tool round, one waiting on its user once it is answered
            how = "queued" if _peer_state(getattr(peer, "status", "")) in ("working", "waiting") else "now"
            if rec is not None:
                rec.last_activity = self._clock()
            log.info("work sessions: the owner's words reached %s (%s, %d characters)", sid[:8], how, len(text))
            return 200, {"ok": True, "delivered": how}
        if state == "held":
            log.info("work sessions: %s holds the owner's words for approval on the Mac", sid[:8])
            return 200, {"ok": True, "delivered": "held", "reason": detail or HELD}
        if state in ("gone", "unsupported") and rec is not None and tty:
            log.info("work sessions: %s has no usable socket (%s); typing into its tab", sid[:8], state)
            return await self._type(rec, tty, text)
        log.info("work sessions: the owner's words did not reach %s (%s)", sid[:8], state)
        if state == "refused":
            return 409, {"ok": False, "reason": detail or REFUSED, "code": "refused"}
        if state == "unsupported":
            return 409, {"ok": False, "reason": NO_SOCKET, "code": "no_socket"}
        return 409, {"ok": False, "reason": detail or UNREACHABLE, "code": "gone" if state == "gone" else "error"}

    async def _type(self, rec: Record, tty: str, text: str) -> tuple[int, dict[str, Any]]:
        """Type into an eve session's Terminal tab: the busy one on the claude process's tty now (TERMINAL_TYPE). One
        line: a newline inside would submit early. The owner's words, never a script's text."""
        line = " ".join(text.split())
        if not tty:
            return 409, {"ok": False, "reason": NO_WINDOW, "code": "no_window"}
        try:
            code, out = await self._osascript(TERMINAL_TYPE, str(rec.window_id or 0), tty, line)
        except OSError:
            code, out = 1, ""
        out = str(out).strip()
        if code != 0 or not (out == "ok" or out.startswith("ok ")):
            log.info("work sessions: reply to %s not typed (%s)", rec.sid[:8], out if out in ("gone", "idle") else "error")
            return 409, {"ok": False, "reason": NO_WINDOW, "code": "no_window"}
        wid = out[2:].strip()
        if wid.isdigit() and int(wid) > 0 and (int(wid) != rec.window_id or tty != rec.tty):
            rec.window_id, rec.tty = int(wid), tty       # where the tab really is, for next time
            self._save()
        rec.last_activity = self._clock()
        log.info("work sessions: reply typed into %s (%d characters)", rec.sid[:8], len(line))
        return 200, {"ok": True, "delivered": "typed"}

    async def stop(self, sid: str) -> tuple[int, dict[str, Any]]:
        rec = self.records.get(sid)
        if rec is None:
            known = self._session(sid) is not None
            if not known:
                seen = await asyncio.to_thread(self._see)
                known = seen.peer(sid) is not None
            return (409, {"ok": False, "reason": NOT_OURS}) if known else (404, {"ok": False, "reason": UNKNOWN})
        if rec.over:                                     # stopped or ended already: the same answer, nothing signalled
            seen = await asyncio.to_thread(self._see)
            if seen.pid(sid) is not None or seen.peer(sid) is not None:
                return 409, {"ok": False, "reason": NOT_OURS}    # reopened at the Mac since: not eve's to stop
            return 200, {"ok": True, "stopped": True}
        pids = await asyncio.to_thread(session_pids, self._run)
        pid = (pids or {}).get(sid)
        if pid is not None:
            with contextlib.suppress(ProcessLookupError, PermissionError):
                self._kill(pid, signal.SIGTERM)
            waited = 0.0
            while waited < STOP_WAIT_SECS:
                await self._sleep(0.25)
                waited += 0.25
                now_pids = await asyncio.to_thread(session_pids, self._run)
                if now_pids is not None and sid not in now_pids:
                    break
            else:
                with contextlib.suppress(ProcessLookupError, PermissionError):
                    self._kill(pid, signal.SIGKILL)
        rec.stopped = True
        self._end(rec)
        self._save()
        state = self._state()
        if state is not None:
            state.session_end(sid)
        log.info("work sessions: %s stopped (%s)", sid[:8], "signalled" if pid is not None else "already gone")
        return 200, {"ok": True, "stopped": True}


def _once(probe: Callable[[], Any]) -> Callable[[], Any]:
    """``probe``, run at most once, and only if asked."""
    box: list[Any] = []

    def get() -> Any:
        if not box:
            box.append(probe())
        return box[0]

    return get


# Markers a running Claude Code session puts in its children's environment. A Work session is a session of its own,
# so launch drops them: Terminal.app keeps the environment of whatever first started it, and a claude that inherits
# CLAUDE_CODE_CHILD_SESSION runs as a child and saves no transcript, which the phone reads (found live 2026-10-20).
INHERITED_SESSION_ENV = ("CLAUDECODE", "CLAUDE_CODE_CHILD_SESSION", "CLAUDE_CODE_SESSION_ID", "CLAUDE_CODE_ENTRYPOINT",
                         "CLAUDE_CODE_EXECPATH", "CLAUDE_CODE_MESSAGING_SOCKET", "CLAUDE_CODE_MESSAGING_TOKEN",
                         "CLAUDE_CODE_SESSION_ATTENDED", "CLAUDE_PID")


# ---- the session's own first moment: python -m eve_bridge.work_sessions launch|resume <sid> ------------------------

def main(argv: Optional[list[str]] = None, *, home: Optional[Path] = None,
         execvp: Callable[[str, list[str]], Any] = os.execvp) -> int:
    """Read and delete the session's task, then become Claude Code (``launch_argv``): ``launch`` starts session
    ``<sid>``, ``resume`` reopens it as itself. Prints nothing of the task."""
    args = sys.argv[1:] if argv is None else list(argv)
    if len(args) != 2 or args[0] not in ("launch", "resume") or not UUID_RE.match(args[1]):
        print("usage: python -m eve_bridge.work_sessions launch|resume <session id>", file=sys.stderr)
        return 2
    verb, sid = args
    folder = files_dir(home)
    task_path = folder / f"{sid}.task"
    try:
        task = task_path.read_text(encoding="utf-8")
    except OSError:
        print("This Work session's task is gone (already started, or never written).", file=sys.stderr)
        return 1
    with contextlib.suppress(OSError):
        task_path.unlink()
    with contextlib.suppress(OSError):
        (folder / f"{sid}.resume").unlink()            # left by a fork made before 2026-10-26; never read again
    claude = shutil.which("claude")
    if claude is None:
        print("Claude Code (claude) is not on this shell's PATH.", file=sys.stderr)
        return 127
    argv_out = launch_argv(sid, task, folder / f"{sid}.mcp.json", claude, resume=verb == "resume")
    for name in INHERITED_SESSION_ENV:
        os.environ.pop(name, None)
    execvp(argv_out[0], argv_out)
    return 0                                            # only reached when execvp is a test's stand-in


if __name__ == "__main__":
    raise SystemExit(main())
