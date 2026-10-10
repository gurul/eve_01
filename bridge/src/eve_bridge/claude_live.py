"""Which Claude Code sessions are still alive, for the "claude on" picker.

The daemon learns of a session from its hooks and forgets it only on SessionEnd. A terminal that is closed,
killed or crashes never sends that, so its session stayed in the picker for good, and joining it typed into
whatever window was in front (docs/stackchan/telegram-bot-api.md, relay weakness 2).

The hooks carry no process id: SessionStart sends the session id, the transcript path and the folder, and
nothing else. So liveness is read from the Mac itself. ``ps`` lists the running ``claude`` processes and
``lsof -d cwd`` gives each one's working folder, both in one call each (about 30 ms together, measured
2026-09-23). A session is alive while a claude process works in its folder, or in a folder above it: the hook's
``cwd`` can be a subfolder the session's shell moved into, while the process stays where it started.

Every doubt keeps a session. The probe failing, or finding no claude process at all (a renamed binary would
look like that), answers "unknown" (None), and the picker then shows what it showed before. A session with a
permission still pending, or one that started moments ago, is never dropped either. Hiding a live session is
worse than showing a dead one: the first loses the owner's way in, the second costs one tap on a stale button.
"""

from __future__ import annotations

import asyncio
import logging
import os
import subprocess
import time
from pathlib import Path
from typing import Any, Callable, Optional

log = logging.getLogger(__name__)

PS = "/bin/ps"
LSOF = "/usr/sbin/lsof"
PROBE_TIMEOUT_SECS = 1.5          # each call; the picker is a reply to a text, not worth a longer wait
GRACE_SECS = 30.0                 # a session this new is kept whatever the probe says

Run = Callable[..., Any]


RUNTIMES = ("node", "bun", "deno")    # what an npm install's ``claude`` shim runs under


def _is_claude(args: str) -> bool:
    """A Claude Code process: the native binary (``claude``, or its full path), the npm install's
    ``node …/claude-code/cli.js``, or that install launched through its bin shim, which ps shows as
    ``node /opt/homebrew/bin/claude`` (review, 2026-09-23: such a session was judged dead and dropped)."""
    words = args.split()
    if not words:
        return False
    if os.path.basename(words[0]) == "claude":
        return True
    if os.path.basename(words[0]) in RUNTIMES and len(words) > 1 and os.path.basename(words[1]) == "claude":
        return True
    return any("claude-code/cli" in w for w in words[1:3])


def live_cwds(run: Run = subprocess.run) -> Optional[set[str]]:
    """The working folders of every running Claude Code process, or None when that cannot be told."""
    try:
        ps = run([PS, "-axww", "-o", "pid=,args="], capture_output=True, text=True, timeout=PROBE_TIMEOUT_SECS)
    except (OSError, subprocess.SubprocessError) as e:
        log.debug("claude_live: ps failed (%s)", type(e).__name__)
        return None
    if ps.returncode != 0:
        return None
    pids = []
    for line in str(ps.stdout).splitlines():
        pid, _, args = line.strip().partition(" ")
        if pid.isdigit() and _is_claude(args.strip()):
            pids.append(pid)
    if not pids:
        return None                                  # no claude at all is more likely a blind probe
    try:
        # -b: never block on a stalled mount; -w: no warnings. A pid gone since ps makes lsof exit 1 and
        # still print the rest, so its output is read whatever the exit code.
        lsof = run([LSOF, "-a", "-b", "-w", "-d", "cwd", "-Fn", "-p", ",".join(pids)],
                   capture_output=True, text=True, timeout=PROBE_TIMEOUT_SECS)
    except (OSError, subprocess.SubprocessError) as e:
        log.debug("claude_live: lsof failed (%s)", type(e).__name__)
        return None
    cwds = {line[1:] for line in str(lsof.stdout).splitlines() if line.startswith("n/")}
    return cwds or None


def _resolved(path: str) -> Path:
    try:
        return Path(path).resolve()
    except (OSError, RuntimeError):
        return Path(path)


def alive(cwd: str, live: set[str]) -> bool:
    """A claude process works in this folder or in one above it."""
    here = _resolved(cwd)
    for folder in live:
        there = _resolved(folder)
        if here == there or there in here.parents:
            return True
    return False


async def picker_sessions_off_loop(state: Any, probe: Optional[Callable[[], Optional[set[str]]]] = None) -> list[str]:
    """``picker_sessions`` for code on the event loop: the probe (two subprocesses, up to 1.5 s each) runs on
    a worker thread, and only the pruning of ``state``, which the loop owns, runs on the loop."""
    live = await asyncio.to_thread(probe or live_cwds)
    return picker_sessions(state, probe=lambda: live)


def picker_sessions(state: Any, live: Optional[set[str]] = None, *,
                    probe: Optional[Callable[[], Optional[set[str]]]] = None,
                    now: Optional[float] = None) -> list[str]:
    """The "claude on" picker's folders, newest first, without the sessions whose process is gone. Those
    are dropped from ``state`` too, as a SessionEnd would have: a hook from one that was alive after all
    brings it straight back (the tool hooks adopt an unknown session)."""
    sessions = sorted(state.sessions.values(), key=lambda s: s.started_at, reverse=True)
    if live is None:
        live = (probe or live_cwds)()
    if live is None:
        return [s.cwd for s in sessions if s.cwd]
    at = time.time() if now is None else now
    keep: list[str] = []
    for s in sessions:
        if not s.cwd:
            continue                                 # no folder: never in the picker, and nothing to judge by
        if alive(s.cwd, live) or at - s.started_at < GRACE_SECS:
            keep.append(s.cwd)
            continue
        state.session_end(s.session_id)
        log.info("claude_live: dropped a session whose Claude process is gone (%s)", Path(s.cwd).name)
    return keep
