"""Muse on the owner's Raspberry Pi: one question in, Muse's answer out (docs/muse.md).

The Pi runs Meta's Muse Linux Device SDK with eve's relay (pi/muse/). eve reaches it over SSH, key login only, and
runs the relay's ``ask.py`` with the message on stdin; ``ask.py`` prints one JSON line, ``{"ok", "reply"}`` or
``{"ok": false, "error"}``. The Pi keeps the Muse connection; nothing Muse-side runs on the Mac.

``EVE_MUSE_PI`` is the SSH target (``user@host``); unset, Muse is off. eve's chat with Muse is one Muse side chat,
its id kept in ``muse-session`` under the config directory, so Muse remembers the conversation across turns.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import re
import uuid
from dataclasses import dataclass
from pathlib import Path

log = logging.getLogger(__name__)

ENV = "EVE_MUSE_PI"
ASK = "/opt/eve-muse/venv-python /opt/eve-muse/ask.py"
SESSION_FILE = "muse-session"
CONFIG_DIR = Path.home() / ".config" / "eve-bridge"
ANSWER_SECS = 170.0          # the Pi waits up to 120 s for Muse; SSH and the socket get the rest
CONNECT_SECS = 6
_TARGET = re.compile(r"^[A-Za-z0-9._-]+@[A-Za-z0-9.:_-]+$")
_SESSION = re.compile(r"^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$")

UNREACHABLE_LINE = "I couldn't reach Muse on the Pi just now."


@dataclass(frozen=True)
class Answer:
    ok: bool
    text: str


def target() -> str:
    """The SSH target from EVE_MUSE_PI, or "" when unset or not a plain user@host."""
    value = os.environ.get(ENV, "").strip()
    return value if _TARGET.fullmatch(value) else ""


def available() -> bool:
    return bool(target())


def session_id(directory: Path | None = None) -> str:
    """eve's Muse side chat: made once, then kept, so Muse keeps the thread."""
    path = (directory or CONFIG_DIR) / SESSION_FILE
    try:
        value = path.read_text().strip()
        if _SESSION.fullmatch(value):
            return value
    except OSError:
        pass
    value = str(uuid.uuid4())
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(value + "\n")
    return value


def command(host: str, session: str) -> list[str]:
    return ["ssh", "-o", "BatchMode=yes", "-o", f"ConnectTimeout={CONNECT_SECS}",
            "-o", "ServerAliveInterval=30", host, f"{ASK} --session-id {session}"]


def parse(stdout: bytes) -> Answer:
    """``ask.py``'s last line: Muse's reply, or why there is none (never the raw output)."""
    lines = [line for line in stdout.decode("utf-8", "replace").splitlines() if line.strip()]
    try:
        reply = json.loads(lines[-1]) if lines else None
    except ValueError:
        reply = None
    if not isinstance(reply, dict):
        return Answer(False, UNREACHABLE_LINE)
    text = reply.get("reply") if isinstance(reply.get("reply"), str) else ""
    if reply.get("ok") and text.strip():
        return Answer(True, text.strip())
    if reply.get("ok"):
        return Answer(False, "Muse didn't answer that.")
    error = str(reply.get("error") or "")
    if "not connected" in error:
        return Answer(False, "Muse isn't connected on the Pi right now.")
    if "in time" in error:
        return Answer(False, text.strip() or "Muse didn't finish answering in time.")
    return Answer(False, UNREACHABLE_LINE)


async def ask(text: str, *, host: str | None = None, session: str | None = None,
              timeout: float = ANSWER_SECS) -> Answer:
    """One message to Muse through the Pi; Muse's answer, or a short line saying why not."""
    host = host if host is not None else target()
    if not host:
        return Answer(False, "Muse is off on this Mac (EVE_MUSE_PI).")
    try:
        proc = await asyncio.create_subprocess_exec(
            *command(host, session or session_id()), stdin=asyncio.subprocess.PIPE,
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
    except OSError as e:
        log.warning("muse: ssh did not start (%s)", type(e).__name__)
        return Answer(False, UNREACHABLE_LINE)
    try:
        stdout, stderr = await asyncio.wait_for(proc.communicate(text.encode()), timeout)
    except asyncio.TimeoutError:
        proc.kill()
        await proc.wait()
        log.warning("muse: no answer from the Pi in %.0f s", timeout)
        return Answer(False, "Muse didn't finish answering in time.")
    answer = parse(stdout)
    log.info("muse: asked (%d chars), %s, ssh exit %s", len(text), "answered" if answer.ok else "no answer",
             proc.returncode)
    if not answer.ok and proc.returncode == 255:
        log.warning("muse: ssh failed: %s", stderr.decode("utf-8", "replace").strip()[:200])
    return answer
