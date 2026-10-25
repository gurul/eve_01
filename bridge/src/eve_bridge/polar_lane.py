"""eve's browser is Polar's own agent (EVE_BROWSER_POLAR=1).

Owner, 2026-10-24: "use only polar". In a blind eval of 16 read-only web tasks, Polar's agent scored 32/32 and
eve's browser lane on Steel 23/32, with median times of 8.8 s and 9.4 s (docs/polar.md). Polar (polar.app, a
Chromium browser with its own agent) is not driven click by click. eve hands it the whole task in one sentence
and reads back its answer:

    web task ─▶ PolarAgent ─▶ local dispatch (one JSON line over TCP on 127.0.0.1)   {"method":"dispatch","query":…}
                           ─▶ Polar's API  /api/v1/threads/<id>             until the run is done
                           ─▶ Polar's API  /api/v1/threads/<id>/messages   the last assistant text is the answer

Polar opens the dispatch listener only when it is launched with ``POLAR_USER_DATA_DIR`` set. That is an
undocumented hook, and a Polar update may remove it. The listener writes ``local-dispatch.json`` (host, port,
pid, authToken) into that folder, and the port changes at every launch, so it is read again for each call.

Polar only: a browser-only web task never falls back to Steel or Codex. When Polar cannot take it, eve says why.

eve's per-step consent (consent.py) cannot see Polar's clicks, so the owner's rule (ask before sending, deleting,
buying) is applied once per task. A task with a consequential verb (task_router.CONSEQUENTIAL) asks first, and
on a yes it goes with a line that allows only what was asked. Every other task goes with a read-only line.
"""
from __future__ import annotations

import asyncio
import json
import logging
import os
import socket
import subprocess
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Awaitable, Callable, Optional

from . import consent
from .agent_contract import AgentEvent
from .task_router import CONSEQUENTIAL

log = logging.getLogger(__name__)

DEFAULT_DIR = "~/Library/Application Support/com.polar.browser.host"
DEFAULT_APP = "/Applications/Polar.app"
DEFAULT_TIMEOUT_SECS = 300.0
POLL_SECS = 1.5
POLL_GIVE_UP_SECS = 30.0      # Polar gone mid-run (quit, crashed, offline): say so instead of waiting out the limit
LAUNCH_WAIT_SECS = 30.0
TOKEN_MAX_AGE_SECS = 300.0
# Polar's API sits behind Cloudflare, which refuses Python's default User-Agent (error 1010).
USER_AGENT = "eve-bridge/polar"
DONE = frozenset({"completed", "failed", "error", "cancelled", "canceled", "stopped"})

READ_ONLY = ("Read only: do not sign in, buy, post, send, delete or use any connected account; "
             "search and lookup forms are fine.")
ALLOWED = ("The owner approved exactly what is asked above. Do only that: nothing else that buys, sends, posts, "
           "deletes or signs in.")


@dataclass(frozen=True)
class PolarConfig:
    enabled: bool
    data_dir: Path
    app: str
    timeout_secs: float


def configured(environ: Any = None) -> PolarConfig:
    """``EVE_BROWSER_POLAR=1`` turns it on. ``EVE_POLAR_DIR`` is Polar's user data folder (the listener's
    ``local-dispatch.json`` lands there), ``EVE_POLAR_APP`` the app, ``EVE_POLAR_TIMEOUT`` one task's limit."""
    env = os.environ if environ is None else environ
    on = (env.get("EVE_BROWSER_POLAR") or "0").strip().lower() in ("1", "true", "yes", "on")
    try:
        timeout = float(env.get("EVE_POLAR_TIMEOUT") or DEFAULT_TIMEOUT_SECS)
    except ValueError:
        timeout = DEFAULT_TIMEOUT_SECS
    return PolarConfig(enabled=on, data_dir=Path(env.get("EVE_POLAR_DIR") or DEFAULT_DIR).expanduser(),
                       app=env.get("EVE_POLAR_APP") or DEFAULT_APP, timeout_secs=max(30.0, timeout))


class PolarError(RuntimeError):
    """Something the owner can act on, in words fit to show them. Never carries a token."""


class PolarClient:
    """Polar's local dispatch listener and its thread API. Blocking: call it from a worker thread."""

    def __init__(self, data_dir: Path, *, connect: Callable[..., Any] = socket.create_connection,
                 urlopen: Callable[..., Any] = urllib.request.urlopen) -> None:
        self.data_dir = Path(data_dir)
        self._connect, self._urlopen = connect, urlopen
        self._cred: Optional[dict[str, Any]] = None
        self._cred_at = 0.0

    def meta(self) -> dict[str, Any]:
        try:
            return json.loads((self.data_dir / "local-dispatch.json").read_text())
        except FileNotFoundError:
            raise PolarError("Polar's eve connection is not open") from None
        except (OSError, ValueError) as e:
            raise PolarError(f"Polar's local-dispatch.json is unreadable ({type(e).__name__})") from None

    def call(self, method: str, **kw: Any) -> Any:
        meta = self.meta()
        try:
            s = self._connect((meta["host"], int(meta["port"])), timeout=15)
        except OSError:
            raise PolarError("Polar's eve connection is not answering") from None
        try:
            s.sendall(json.dumps({"authToken": meta["authToken"], "method": method, **kw}).encode() + b"\n")
            buf = b""
            while not buf.endswith(b"\n"):
                chunk = s.recv(65536)
                if not chunk:
                    break
                buf += chunk
        except OSError as e:
            raise PolarError(f"Polar's eve connection dropped ({type(e).__name__})") from None
        finally:
            s.close()
        try:
            reply = json.loads(buf)
        except ValueError:
            raise PolarError("Polar answered something eve cannot read") from None
        if isinstance(reply, dict) and reply.get("error"):
            err = reply["error"]
            text = err.get("message") if isinstance(err, dict) else err
            raise PolarError(f"Polar refused {method}: {str(text)[:160]}")
        return reply.get("result") if isinstance(reply, dict) else reply

    def get(self, path: str) -> Any:
        if self._cred is None or time.monotonic() - self._cred_at > TOKEN_MAX_AGE_SECS:
            self._cred = self.call("token")
            self._cred_at = time.monotonic()
        req = urllib.request.Request(self._cred["apiBaseURL"] + path, headers={
            "Authorization": "Bearer " + self._cred["token"], "Accept": "application/json",
            "User-Agent": USER_AGENT})
        try:
            with self._urlopen(req, timeout=20) as r:
                return json.loads(r.read())
        except urllib.error.HTTPError as e:
            if e.code == 401:
                self._cred = None                    # a stale token: the next call mints a new one
            raise PolarError(f"Polar's server answered {e.code}") from None
        except (urllib.error.URLError, OSError, ValueError) as e:
            raise PolarError(f"Polar's server is unreachable ({type(e).__name__})") from None

    def status(self) -> dict[str, Any]:
        return self.call("status") or {}

    def dispatch(self, query: str) -> str:
        res = self.call("dispatch", query=query) or {}
        tid = res.get("threadId") if isinstance(res, dict) else None
        if not tid:
            raise PolarError("Polar did not start the task")
        return str(tid)

    def stop(self, thread_id: str) -> None:
        self.call("stop", threadId=thread_id)

    def run_status(self, thread_id: str) -> str:
        return str((self.get(f"/api/v1/threads/{thread_id}") or {}).get("latest_run_status") or "")

    def messages(self, thread_id: str) -> list[dict[str, Any]]:
        m = self.get(f"/api/v1/threads/{thread_id}/messages")
        items = m if isinstance(m, list) else (m.get("messages") or m.get("data") or m.get("items") or [])
        return [it for it in items if isinstance(it, dict)]


def assistant_parts(messages: list[dict[str, Any]]) -> tuple[list[str], list[str]]:
    """(the assistant's texts, the names of its tool calls), in order."""
    texts: list[str] = []
    tools: list[str] = []
    for it in messages:
        d = it.get("data") or {}
        if d.get("role") != "assistant":
            continue
        for c in d.get("content") or []:
            if not isinstance(c, dict):
                continue
            if c.get("type") == "text" and c.get("text"):
                texts.append(str(c["text"]))
            elif c.get("type") == "tool_call":
                tools.append(str(c.get("name") or "tool"))
    return texts, tools


def polar_running(run: Callable[..., Any] = subprocess.run) -> bool:
    """Whether Polar's app process is up (with or without eve's connection)."""
    try:
        out = run(["pgrep", "-f", "Polar.app/Contents/MacOS/Polar"], capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError):
        return False
    return bool((out.stdout or "").strip())


def launch_command(cfg: PolarConfig) -> list[str]:
    """Polar in the background (-g) with the listener on, on the owner's own profile folder."""
    return ["open", "-g", "--env", f"POLAR_USER_DATA_DIR={cfg.data_dir}", "-a", cfg.app]


def ensure_ready(cfg: PolarConfig, client: PolarClient, *, run: Callable[..., Any] = subprocess.run,
                 sleep: Callable[[float], None] = time.sleep, clock: Callable[[], float] = time.monotonic) -> None:
    """Blocking. Polar answering, signed in and ready, or a PolarError saying what to do. Starts Polar with the
    connection when it is not running. A Polar that is running without the connection is the owner's window:
    it is never quit here. A Polar just started answers before it is ready (live, 2026-10-24), so this waits
    up to LAUNCH_WAIT_SECS for ready."""
    st: Optional[dict[str, Any]]
    try:
        st = client.status()
    except PolarError:
        if polar_running(run):
            raise PolarError("Polar is open without eve's connection. Quit Polar, and eve starts it with the "
                             "connection on the next web task") from None
        log.info("polar: starting Polar with eve's connection")
        try:
            run(launch_command(cfg), capture_output=True, text=True, timeout=15)
        except (OSError, subprocess.SubprocessError) as e:
            raise PolarError(f"Polar did not start ({type(e).__name__})") from None
        st = None
    deadline = clock() + LAUNCH_WAIT_SECS
    while st is None or not st.get("isReady"):
        if clock() > deadline:
            break
        sleep(1.0)
        try:
            st = client.status()
        except PolarError:
            st = None
    if st is None:
        raise PolarError(f"Polar's eve connection did not open within {LAUNCH_WAIT_SECS:.0f} s")
    if not st.get("isAuthenticated"):
        raise PolarError("Polar is not signed in. Open Polar and sign in")
    if not st.get("isReady"):
        raise PolarError(f"Polar was not ready within {LAUNCH_WAIT_SECS:.0f} s")


class PolarAgent:
    """One web task, done by Polar's agent. The run/steer/cancel/status contract of chrome_lane.ChromeLaneAgent,
    so it drops in as app_reflex.ReflexFirstAgent's second body (make_auto)."""

    provider = "polar"

    def __init__(self, cfg: PolarConfig, on_event: Callable[[AgentEvent], None],
                 ask_user: Optional[Callable[[str], Awaitable[str]]], *, client: Optional[PolarClient] = None,
                 ready: Optional[Callable[[], None]] = None, poll_secs: float = POLL_SECS) -> None:
        self.cfg = cfg
        self.client = client or PolarClient(cfg.data_dir)
        self._ready = ready or (lambda: ensure_ready(cfg, self.client))
        self.on_event, self.ask_user = on_event, ask_user
        self._poll = poll_secs
        self._cancel = asyncio.Event()
        self._cancel_reason = ""
        self.running = False
        self.goal = self.final = ""
        self.thread_id = ""
        self.handed_on = False
        self.browser_used = True

    def status(self) -> dict[str, Any]:
        return {"provider": self.provider, "running": self.running, "goal": self.goal, "thread": self.thread_id,
                "handed_on": False}

    def steer(self, text: str) -> bool:
        return False                                   # a dispatched Polar run takes no mid-task words

    def cancel(self, reason: str = "") -> None:
        self._cancel_reason = reason or "stopped"
        self._cancel.set()

    async def browser_shot(self) -> None:
        return None                                    # Polar's tabs are its own; eve has no picture of them

    def _finish(self, kind: str, text: str) -> str:
        self.final = text
        self.running = False
        self.on_event(AgentEvent(kind, text))
        return text

    async def run(self, goal: str) -> str:
        self.goal, self.running = goal, True
        self._cancel.clear()
        self.on_event(AgentEvent("started", goal))
        line = READ_ONLY
        if CONSEQUENTIAL.search(goal or ""):
            if self.ask_user is None:
                return self._finish("final", "Not done: this would act in your Polar account, and nobody is "
                                             "here to say yes.")
            reply = await self.ask_user(f"Polar would do this in your Polar account, and eve can't check each "
                                        f"click: “{goal}”. Go ahead?")
            if consent.decision(reply) != "allow":
                return self._finish("final", "Not done: you didn't say yes.")
            line = ALLOWED
        try:
            await asyncio.to_thread(self._ready)
            self.thread_id = await asyncio.to_thread(self.client.dispatch, f"{goal}\n\n{line}")
        except PolarError as e:
            log.warning("polar: %s", e)
            return self._finish("error", f"Polar couldn't take this: {e}.")
        log.info("polar: task sent (thread %s)", self.thread_id[:12])
        t0 = time.monotonic()
        seen, status = 0, ""
        failing_since: Optional[float] = None
        while True:
            try:
                await asyncio.wait_for(self._cancel.wait(), timeout=self._poll)
            except asyncio.TimeoutError:
                pass
            if self._cancel.is_set():
                await self._stop()
                return self._finish("cancelled", "Stopped.")
            if time.monotonic() - t0 > self.cfg.timeout_secs:
                await self._stop()
                return self._finish("error", f"Polar didn't finish within {self.cfg.timeout_secs:.0f} s, "
                                             f"so eve stopped it.")
            try:
                status = await asyncio.to_thread(self.client.run_status, self.thread_id)
                texts, tools = assistant_parts(await asyncio.to_thread(self.client.messages, self.thread_id))
            except PolarError as e:
                log.info("polar: poll: %s", e)          # a missed poll is retried on the next tick
                failing_since = failing_since or time.monotonic()
                if time.monotonic() - failing_since > POLL_GIVE_UP_SECS:
                    return self._finish("error", f"Lost touch with Polar mid-task: {e}.")
                continue
            failing_since = None
            for name in tools[seen:]:
                self.on_event(AgentEvent("progress", f"Polar: {name.replace('_', ' ')}"))
            seen = len(tools)
            if status in DONE:
                break
        log.info("polar: %s in %.1f s, %d tool call(s)", status, time.monotonic() - t0, seen)
        if status == "completed" and texts:
            return self._finish("final", texts[-1])
        return self._finish("error", f"Polar's run ended {status or 'without an answer'}"
                                     + (f": {texts[-1]}" if texts else "."))

    async def _stop(self) -> None:
        if not self.thread_id:
            return
        try:
            await asyncio.to_thread(self.client.stop, self.thread_id)
        except PolarError as e:
            log.info("polar: stop: %s", e)
