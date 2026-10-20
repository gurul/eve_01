"""The Work tab's sessions: Claude Code in ``~/Documents/personal/work``, started and driven from the phone.

Owner, 2026-10-12: "can work essentially be a new terminal session in documents personal", "since its long
horizon", "make a folder called work under personal where this happens". Each session is plain
``claude --dangerously-skip-permissions`` in that one folder (``WORK_DIR``; ``EVE_WORK_ROOT`` moves it), never
claude_launch's "work" *area* (``~/Documents/work`` with era-code). The routes are link_chat.py's
``/api/link/sessions*``; the whole design is in docs/work-sessions.md.

**Start.** ``start(task, client_id)`` mints the session's id (a UUID), writes the task to ``<sid>.task`` and eve's
MCP config to ``<sid>.mcp.json`` (both 0600, in ``~/.config/eve-bridge/work/``, 0700), and opens one Terminal.app
window whose only line is code-made: ``cd <WORK_DIR> && exec <python> -P -m eve_bridge.work_sessions launch <sid>`` (``-P``: the shared work folder,
where sessions write files, is never on ``sys.path``; the MCP server's args carry it too).
That line is passed to osascript as an argument, never written into the script's source. ``launch`` (``main``)
reads and deletes the task file and replaces itself with ``claude``, the task as one argv element after ``--``
(``launch_argv``): the task never passes through a shell or an AppleScript string, whatever it holds. The same
``client_id`` within ``DEDUPE_SECS`` answers the first start's session; nothing else opens.

**Which session is which.** Every session shares one folder, so the folder cannot tell them apart. The id can:
``claude --session-id <sid>`` makes Claude Code's own session id ours, so the hooks' ``session_id`` (``note``) and
``ps`` (``session_pids``) both name it. ``ps`` finding the process is liveness and the stop target. A ``/clear``
inside the session goes on under a new session id in the same process: the row stays (ps still names ours), its
state and detail stop following, and the new id is listed beside it as a session eve did not start.

**Reply.** Terminal.app's ``do script <text> in <tab>`` types into that one tab, found by its window id and its tty
(kept at start), with the text as an osascript argument. It is refused unless ``ps`` finds the session's claude and
the tab is busy (a program, not a shell prompt, holds it; ``exec`` means no shell is left once claude exits). No
other window is touched and nothing needs focus. Warp has no AppleScript tab addressing, so Work sessions open in
Terminal.app whatever ``EVE_CLAUDE_TERMINAL`` says for "new claude".

**Stop.** SIGTERM to the pid ``ps`` gives for ``--session-id <sid>``, SIGKILL after ``STOP_WAIT_SECS``. Idempotent.

**The list** is every live Claude Code session the daemon knows (``state.sessions``) plus the Work ones; only Work
sessions can be replied to or stopped. A session's state: waiting on you (a Notification that waits, or an
AskUserQuestion), working (between a prompt and Stop), idle, or starting (opened, no hook yet).

**Detail** answers state "ended" (and ``ended: true``) for a session that is over, and ends one whose process ``ps``
no longer finds after ``GRACE_SECS``, as the list does. A 409 for an ended session carries ``code: "ended"``.

**Continue** (``continue_session``): a session eve did not start, or a Work session that has ended, is jumped into
from the phone by forking it. A new Work session runs ``claude --resume <sid> --fork-session --session-id <new>`` in
the original's folder, the owner's words its first prompt: the whole conversation under a new id that ps names, so
reply and stop work on it as on any Work session. The original (in Warp, an IDE, wherever) is left as it was; Warp
cannot be typed into by tab, so a fork is the one way in that works for every terminal.

**Detail** reads the transcript's last ``TAIL_BYTES`` on a thread: the owner's prompts and Claude's text, never
tool calls, tool results or thinking, each clipped to ``TURN_CHARS`` with token-shaped strings and ``KEY=value``
secrets masked (``redact``). Folders are basenames only.

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
from typing import Any, Callable, Optional

from . import claude_live
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
TAIL_BYTES = 512 * 1024                  # detail reads at most this much of the transcript's end
LINE_TAIL_BYTES = 64 * 1024              # the list's last line, from at most this much
DEDUPE_SECS = 600.0                      # the same client_id within this is the same session
GRACE_SECS = 30.0                        # a session this new is "starting" whatever ps says
STOP_WAIT_SECS = 5.0                     # SIGTERM, then SIGKILL after this
KEEP_SECS = 7 * 24 * 3600                # an ended session's record is kept this long
MAX_LISTED = 30
STATES = ("starting", "working", "waiting", "idle")      # live states; "ended" only in detail
ENDED_CODE = "ended"                     # the "code" beside ENDED, so the app needs no display text to match
OFF_LINE = "Work sessions are off on this Mac (EVE_WORK_SESSIONS=0)."
ENDED = "that session has ended"
NO_WINDOW = "can't reach that session's window"
NOT_OURS = "eve can only reply to or stop sessions it started from Work"
NO_CONTINUE = "eve can't find that session's folder or transcript, so it can't continue it"
LIVE_WORK = "that session takes replies as it is; continue is for one that can't"
UNKNOWN = "there is no such session"

# Terminal.app, with every value an argument (``on run argv``), never part of the script's text.
# Opens one window running item 1, and answers "<window id> <tty>" for the tab it made.
# "0" for a window id it could not read: the window is open, but replies cannot find it.
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
# Types item 3 into the tab of window item 1 whose tty is item 2, only while a program holds it (busy).
TERMINAL_TYPE = '''on run argv
  set wid to (item 1 of argv) as integer
  tell application "Terminal"
    if not (exists window id wid) then return "gone"
    repeat with t in tabs of window id wid
      if (tty of t) is (item 2 of argv) then
        if not (busy of t) then return "idle"
        do script (item 3 of argv) in t
        return "ok"
      end if
    end repeat
    return "gone"
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
    restart): the derived path, else ``<sid>.jsonl`` in any project folder. None when there is none."""
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
                     chars: int = TURN_CHARS) -> tuple[list[dict[str, Any]], bool]:
    """The last ``limit`` turns: ``{"role": "owner" | "claude", "text", "at"}``, oldest first, and whether more came
    before. The owner's typed prompts and Claude's text only: subagents' lines, meta lines, command and system
    wrappers (text that starts with "<"), tool calls, tool results and thinking are left out."""
    if path is None:
        return [], False
    objs, cut = read_tail(path, tail)
    turns: list[dict[str, Any]] = []
    for obj in objs:
        if obj.get("isSidechain") or obj.get("isMeta"):
            continue
        msg = obj.get("message") if isinstance(obj.get("message"), dict) else None
        if msg is None:
            continue
        role = msg.get("role")
        if role not in ("user", "assistant") or obj.get("type") not in (None, "user", "assistant"):
            continue
        for text in _texts(msg.get("content"), role):
            text = text.strip()
            if not text or (role == "user" and text.startswith("<")):
                continue
            turns.append({"role": "owner" if role == "user" else "claude", "text": clip(redact(text), chars),
                          "at": int(_epoch(obj.get("timestamp")))})
    return turns[-limit:], cut or len(turns) > limit


def last_line(path: Optional[Path]) -> str:
    """Claude's newest text in the transcript, one line, at most LINE_CHARS."""
    turns, _ = transcript_turns(path, limit=TURNS_SHOWN, tail=LINE_TAIL_BYTES, chars=LINE_CHARS * 2)
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


# ---- the process ---------------------------------------------------------------------------------------------------

def session_pids(run: Callable[..., Any] = subprocess.run) -> Optional[dict[str, int]]:
    """Each running Claude Code process started with ``--session-id <uuid>`` → its pid; None when ps fails."""
    try:
        ps = run([claude_live.PS, "-axww", "-o", "pid=,args="], capture_output=True, text=True,
                 timeout=claude_live.PROBE_TIMEOUT_SECS)
    except (OSError, subprocess.SubprocessError) as e:
        log.debug("work sessions: ps failed (%s)", type(e).__name__)
        return None
    if ps.returncode != 0:
        return None
    out: dict[str, int] = {}
    for line in str(ps.stdout).splitlines():
        pid, _, args = line.strip().partition(" ")
        if not pid.isdigit() or not claude_live._is_claude(args.strip()):
            continue
        words = args.split()
        for i, word in enumerate(words):
            if word == "--":
                break                                     # what follows is the task, never a flag
            sid = ""
            if word == "--session-id" and i + 1 < len(words):
                sid = words[i + 1]
            elif word.startswith("--session-id="):
                sid = word.partition("=")[2]
            if UUID_RE.match(sid):
                out[sid] = int(pid)
                break
    return out


def launch_argv(sid: str, task: str, mcp_config: Path, claude: str = "claude", resume: str = "") -> list[str]:
    """Claude Code's argv for a Work session. ``--mcp-config`` takes several values, so ``--session-id`` follows it,
    and ``--`` ends the options: the task is the one positional argument (the first prompt), even when it starts
    with a dash. ``resume``: a session this one continues, forked (``--resume <it> --fork-session``), so the new
    session has the whole conversation under its own id and the original is left as it was."""
    if not UUID_RE.match(sid) or (resume and not UUID_RE.match(resume)):
        raise ValueError("not a session id")
    fork = ["--resume", resume, "--fork-session"] if resume else []
    return [claude, "--dangerously-skip-permissions", "--mcp-config", str(mcp_config), *fork, "--session-id", sid,
            "--", task]


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


# ---- the registry --------------------------------------------------------------------------------------------------

SAVED_FIELDS = ("sid", "created_at", "title", "token_hash", "client_id", "window_id", "tty", "transcript_path",
                "stopped", "ended_at", "cwd", "resumed_from")


@dataclass
class Record:
    sid: str
    created_at: float
    title: str
    token_hash: str
    client_id: str = ""
    window_id: Optional[int] = None
    tty: str = ""
    transcript_path: str = ""
    stopped: bool = False
    ended_at: float = 0.0
    cwd: str = ""                                 # its folder; "" is the work folder
    resumed_from: str = ""                        # the session it continues (forked), if any
    phase: str = "starting"                       # not saved: starting, working, waiting, idle
    last_activity: float = 0.0                    # not saved: the newest hook

    @property
    def over(self) -> bool:
        return self.stopped or self.ended_at > 0

    def saved(self) -> dict[str, Any]:
        d = asdict(self)
        return {k: d[k] for k in SAVED_FIELDS}


class WorkSessions:
    """The Work sessions eve started, and the routes' logic. Everything that touches the Mac is injectable:
    ``osascript`` (script, *argv) → (code, out), ``run`` for ps, ``kill`` for signals, ``live`` for the claude
    processes' folders (claude_live.live_processes)."""

    def __init__(self, *, root: Optional[Path] = None, home: Optional[Path] = None,
                 state: Callable[[], Any] = lambda: None, osascript: Callable[..., Any] = _osascript,
                 run: Callable[..., Any] = subprocess.run, kill: Callable[[int, int], None] = os.kill,
                 live: Optional[Callable[[], Optional[dict[str, bool]]]] = None,
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
        self._clock = clock
        self.python = python
        self._sleep = sleep
        self._environ = environ
        self._claude_dir = claude_dir
        self._lock = asyncio.Lock()
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
        """A hook event for any session; only Work sessions are recorded. Never raises."""
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

    def check_token(self, sid: Any, token: Any) -> bool:
        """Whether ``token`` is the eve MCP token of a Work session that has not ended (constant time)."""
        rec = self.records.get(sid) if isinstance(sid, str) else None
        if rec is None or rec.over or not isinstance(token, str) or not token or not rec.token_hash:
            return False
        return hmac.compare_digest(_hash(token), rec.token_hash)

    # -- helpers --
    def _folder(self, rec: Record) -> Path:
        return Path(rec.cwd) if rec.cwd else self.root

    def _transcript(self, rec: Optional[Record], session: Any = None, sid: str = "") -> Optional[Path]:
        if rec is not None and rec.transcript_path:
            return Path(rec.transcript_path)
        path = getattr(session, "transcript_path", None)
        if isinstance(path, str) and path:
            return Path(path)
        if rec is not None:
            return derived_transcript(self._folder(rec), rec.sid, self._claude_dir)
        if session is not None:
            return found_transcript(getattr(session, "cwd", None), getattr(session, "session_id", "") or sid,
                                    self._claude_dir)
        return None

    def _probe(self) -> tuple[Optional[dict[str, int]], Optional[dict[str, bool]]]:
        return session_pids(self._run), self._live()

    def _end(self, rec: Record) -> None:
        if not rec.over:
            rec.ended_at = self._clock()
            self._save()
        with contextlib.suppress(OSError):
            (self.files / f"{rec.sid}.mcp.json").unlink()
        with contextlib.suppress(OSError):
            (self.files / f"{rec.sid}.task").unlink()
        with contextlib.suppress(OSError):
            (self.files / f"{rec.sid}.resume").unlink()

    def _row(self, rec: Record, pids: Optional[dict[str, int]], path: Optional[Path], line: str) -> dict[str, Any]:
        alive = pids is None or rec.sid in pids
        state = rec.phase if rec.phase in STATES else "idle"
        if pids is not None and rec.sid not in pids and self._clock() - rec.created_at < GRACE_SECS:
            state = "starting"                          # within its grace: opened, claude not up yet
        return {"id": rec.sid, "folder": self._folder(rec).name, "title": rec.title,
                "state": state, "last_activity": int(max(rec.last_activity, rec.created_at, _mtime(path))),
                "last_line": line, "from_work": True,
                "can_reply": alive and rec.window_id is not None and bool(rec.tty), "can_stop": True,
                "can_continue": False}

    @staticmethod
    def _continuable(cwd: Any, path: Optional[Path]) -> bool:
        """A session another can fork: its folder is there and so is its transcript."""
        return bool(cwd) and Path(str(cwd)).is_dir() and path is not None and path.is_file()

    def _rows(self, records: list[Record], sessions: list[Any], pids: Optional[dict[str, int]],
              live: Optional[dict[str, bool]]) -> tuple[list[dict[str, Any]], list[Record]]:
        """(the rows, Work records whose process is gone). Runs on a thread (it reads transcripts), so it gets
        snapshots of the records and the daemon's sessions, taken on the loop."""
        now = self._clock()
        rows: list[dict[str, Any]] = []
        gone: list[Record] = []
        ours = {r.sid for r in records}
        for rec in records:
            if rec.over:
                continue
            if pids is not None and rec.sid not in pids and now - rec.created_at >= GRACE_SECS:
                gone.append(rec)
                continue
            path = self._transcript(rec)
            rows.append(self._row(rec, pids, path, last_line(path)))
        state = self._state()
        for s in sessions:
            sid = getattr(s, "session_id", "")
            if sid in ours or not getattr(s, "cwd", None):
                continue
            if live is not None and not claude_live.alive(s.cwd, set(live)) and now - s.started_at >= GRACE_SECS:
                continue
            path = self._transcript(None, s)
            waiting = bool(state is not None and state._needs_input_live(s))
            rows.append({"id": sid, "folder": Path(s.cwd).name, "title": Path(s.cwd).name,
                         "state": "waiting" if waiting else "working" if s.running else "idle",
                         "last_activity": int(max(s.started_at, _mtime(path))), "last_line": last_line(path),
                         "from_work": False, "can_reply": False, "can_stop": False,
                         "can_continue": self._continuable(s.cwd, path)})
        rows.sort(key=lambda r: r["last_activity"], reverse=True)
        return rows[:MAX_LISTED], gone

    # -- the routes: each answers (HTTP status, JSON body) --
    async def list(self) -> tuple[int, dict[str, Any]]:
        state = self._state()
        sessions = list(getattr(state, "sessions", {}).values()) if state is not None else []
        pids, live = await asyncio.to_thread(self._probe)
        rows, gone = await asyncio.to_thread(self._rows, list(self.records.values()), sessions, pids, live)
        for rec in gone:
            log.info("work sessions: %s is gone (no claude process)", rec.sid[:8])
            self._end(rec)
        return 200, {"ok": True, "folder": self.root.name, "sessions": rows}

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
                        return 200, {"ok": True, "session": self._row(rec, None, self._transcript(rec), "")}
            try:
                self.root.mkdir(parents=True, exist_ok=True)
            except OSError as e:
                return 503, {"ok": False, "reason": f"the work folder is not there ({type(e).__name__})"}
            rec, problem = await self._open(task, client_id, now)
            if rec is None:
                return 503, {"ok": False, "reason": problem}
            log.info("work sessions: %s started (task %d characters)", rec.sid[:8], len(task))
            return 200, {"ok": True, "session": self._row(rec, None, None, "")}

    async def _open(self, task: str, client_id: str, now: float, *, cwd: str = "", resume: str = "",
                    title: str = "") -> tuple[Optional[Record], str]:
        """Write a new session's files and open its Terminal window: (the record, "") or (None, why not). The
        caller holds the lock. ``cwd``: its folder ("" is the work folder); ``resume``: the session it forks."""
        sid = str(uuid.uuid4())
        token = secrets.token_urlsafe(32)
        rec = Record(sid=sid, created_at=now, title=title or flat(task.splitlines()[0], TITLE_CHARS),
                     token_hash=_hash(token), client_id=client_id, cwd=cwd, resumed_from=resume)
        rec.last_activity = now
        try:
            _write_private(self.files / f"{sid}.task", task)
            if resume:
                _write_private(self.files / f"{sid}.resume", resume)
            _write_private(self.files / f"{sid}.mcp.json",
                           json.dumps(mcp_config(sid, token, self.python, self._environ)))
        except OSError as e:
            log.warning("work sessions: could not write the session's files (%s)", type(e).__name__)
            return None, "eve couldn't write the session's files"
        self.records[sid] = rec
        self._save()
        # Only code-made words: the folder (quoted), this Python (quoted), the module and a checked UUID.
        # -P: Python leaves the cwd (the folder, where sessions write files) off sys.path, so a json.py or
        # uuid.py a session left there cannot shadow the stdlib or run in this interpreter.
        line = (f"cd {shlex.quote(str(self._folder(rec)))} && exec {shlex.quote(self.python)} "
                f"-P -m eve_bridge.work_sessions launch {sid}")
        try:
            code, out = await self._osascript(TERMINAL_OPEN, line)
        except OSError as e:
            code, out = 1, type(e).__name__
        if code != 0:
            log.warning("work sessions: Terminal did not open (exit %d)", code)
            del self.records[sid]
            self._end(rec)
            self._save()
            return None, "eve couldn't open Terminal on the Mac"
        wid, _, tty = str(out).strip().partition(" ")
        # the window opened; without its id or tty the session runs, and replies say they can't reach it
        rec.window_id = int(wid) if wid.isdigit() and int(wid) > 0 else None
        rec.tty = tty.strip() if tty.strip().startswith("/dev/") else ""
        self._save()
        return rec, ""

    async def continue_session(self, sid: str, text: Any, client_id: Any = "") -> tuple[int, dict[str, Any]]:
        """Jump into a session from the phone: one eve did not start (in Warp, an IDE, another terminal) or a Work
        session that has ended. A new Work session forks it (``claude --resume <sid> --fork-session``) in the same
        folder, ``text`` its first prompt, and takes replies from then on. The original is left as it was."""
        if not isinstance(text, str) or not text.strip():
            return 400, {"ok": False, "reason": "say what Claude should do next"}
        text = text.strip()
        if len(text) > MAX_TASK_CHARS:
            return 400, {"ok": False, "reason": f"at most {MAX_TASK_CHARS} characters"}
        if client_id is None:
            client_id = ""
        if not isinstance(client_id, str) or len(client_id) > MAX_CLIENT_ID_CHARS:
            return 400, {"ok": False, "reason": "client_id must be a string of at most 64 characters"}
        async with self._lock:
            now = self._clock()
            if client_id:                               # a retry of the same send is the session it already made
                for other in self.records.values():
                    if other.client_id == client_id and now - other.created_at < DEDUPE_SECS:
                        return 200, {"ok": True, "session": self._row(other, None, self._transcript(other), "")}
            rec = self.records.get(sid)
            state = self._state()
            session = getattr(state, "sessions", {}).get(sid) if state is not None else None
            if rec is None and session is None:
                return 404, {"ok": False, "reason": UNKNOWN}
            if rec is not None and not rec.over:
                pids = await asyncio.to_thread(session_pids, self._run)
                if pids is None or sid in pids or now - rec.created_at < GRACE_SECS:
                    return 409, {"ok": False, "reason": LIVE_WORK}
                self._end(rec)
            cwd = str(self._folder(rec)) if rec is not None else str(getattr(session, "cwd", "") or "")
            path = self._transcript(rec, session, sid)
            if not await asyncio.to_thread(self._continuable, cwd, path):
                return 409, {"ok": False, "reason": NO_CONTINUE}
            title = rec.title if rec is not None and rec.title else Path(cwd).name
            new, problem = await self._open(text, client_id, now, cwd=cwd, resume=sid, title=title)
            if new is None:
                return 503, {"ok": False, "reason": problem}
        log.info("work sessions: %s continues %s (%d characters)", new.sid[:8], sid[:8], len(text))
        return 200, {"ok": True, "session": self._row(new, None, None, "")}

    async def detail(self, sid: str) -> tuple[int, dict[str, Any]]:
        rec = self.records.get(sid)
        state = self._state()
        session = getattr(state, "sessions", {}).get(sid) if state is not None else None
        if rec is None and session is None:
            return 404, {"ok": False, "reason": UNKNOWN}
        path = self._transcript(rec, session)
        turns, cut = await asyncio.to_thread(transcript_turns, path)
        if rec is not None:
            pids = await asyncio.to_thread(session_pids, self._run)
            if (not rec.over and pids is not None and sid not in pids
                    and self._clock() - rec.created_at >= GRACE_SECS):
                log.info("work sessions: %s is gone (no claude process)", rec.sid[:8])
                self._end(rec)                          # claude exited (/exit, a crash): the sheet shows it ended
            row = self._row(rec, pids, path, "")
            if rec.over:
                row.update(state="ended", can_reply=False, can_stop=False,
                           can_continue=await asyncio.to_thread(self._continuable, str(self._folder(rec)), path))
        else:
            row = {"id": sid, "folder": Path(session.cwd or "").name, "title": Path(session.cwd or "").name,
                   "state": "waiting" if state._needs_input_live(session) else "working" if session.running
                   else "idle", "last_activity": int(max(session.started_at, _mtime(path))),
                   "from_work": False, "can_reply": False, "can_stop": False,
                   "can_continue": await asyncio.to_thread(self._continuable, session.cwd, path)}
        last = next((t["text"] for t in reversed(turns) if t["role"] == "claude"), "")
        row["last_line"] = flat(last, LINE_CHARS)
        return 200, {"ok": True, "session": row, "turns": turns, "cut": cut, "ended": bool(rec and rec.over)}

    async def reply(self, sid: str, text: Any) -> tuple[int, dict[str, Any]]:
        rec = self.records.get(sid)
        if rec is None:
            state = self._state()
            known = state is not None and sid in getattr(state, "sessions", {})
            return (409, {"ok": False, "reason": NOT_OURS}) if known else (404, {"ok": False, "reason": UNKNOWN})
        if not isinstance(text, str) or not text.strip():
            return 400, {"ok": False, "reason": "nothing to send"}
        # One line: a newline inside would submit early. The owner's words, never a script's text.
        line = " ".join(text.split())
        if len(line) > MAX_REPLY_CHARS:
            return 400, {"ok": False, "reason": f"at most {MAX_REPLY_CHARS} characters"}
        if rec.over:
            return 409, {"ok": False, "reason": ENDED, "code": ENDED_CODE}
        pids = await asyncio.to_thread(session_pids, self._run)
        if pids is None:
            return 409, {"ok": False, "reason": NO_WINDOW}
        if sid not in pids:
            if self._clock() - rec.created_at >= GRACE_SECS:
                self._end(rec)
                return 409, {"ok": False, "reason": ENDED, "code": ENDED_CODE}
            return 409, {"ok": False, "reason": "that session is still starting; try again in a moment"}
        if rec.window_id is None or not rec.tty:
            return 409, {"ok": False, "reason": NO_WINDOW}
        try:
            code, out = await self._osascript(TERMINAL_TYPE, str(rec.window_id), rec.tty, line)
        except OSError:
            code, out = 1, ""
        if code != 0 or out != "ok":
            log.info("work sessions: reply to %s not typed (%s)", sid[:8], out if out in ("gone", "idle") else "error")
            return 409, {"ok": False, "reason": NO_WINDOW}
        rec.last_activity = self._clock()
        log.info("work sessions: reply typed into %s (%d characters)", sid[:8], len(line))
        return 200, {"ok": True}

    async def stop(self, sid: str) -> tuple[int, dict[str, Any]]:
        rec = self.records.get(sid)
        if rec is None:
            state = self._state()
            known = state is not None and sid in getattr(state, "sessions", {})
            return (409, {"ok": False, "reason": NOT_OURS}) if known else (404, {"ok": False, "reason": UNKNOWN})
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


# ---- the session's own first moment: python -m eve_bridge.work_sessions launch <sid> --------------------------------

def main(argv: Optional[list[str]] = None, *, home: Optional[Path] = None,
         execvp: Callable[[str, list[str]], Any] = os.execvp) -> int:
    """Read and delete the session's task, then become Claude Code (``launch_argv``). Prints nothing of the task."""
    args = sys.argv[1:] if argv is None else list(argv)
    if len(args) != 2 or args[0] != "launch" or not UUID_RE.match(args[1]):
        print("usage: python -m eve_bridge.work_sessions launch <session id>", file=sys.stderr)
        return 2
    sid = args[1]
    folder = files_dir(home)
    task_path = folder / f"{sid}.task"
    try:
        task = task_path.read_text(encoding="utf-8")
    except OSError:
        print("This Work session's task is gone (already started, or never written).", file=sys.stderr)
        return 1
    with contextlib.suppress(OSError):
        task_path.unlink()
    resume = ""
    resume_path = folder / f"{sid}.resume"
    with contextlib.suppress(OSError):
        resume = resume_path.read_text(encoding="utf-8").strip()
        resume_path.unlink()
    if resume and not UUID_RE.match(resume):
        print("This Work session's resume file does not name a session.", file=sys.stderr)
        return 1
    claude = shutil.which("claude")
    if claude is None:
        print("Claude Code (claude) is not on this shell's PATH.", file=sys.stderr)
        return 127
    argv_out = launch_argv(sid, task, folder / f"{sid}.mcp.json", claude, resume)
    execvp(argv_out[0], argv_out)
    return 0                                            # only reached when execvp is a test's stand-in


if __name__ == "__main__":
    raise SystemExit(main())
