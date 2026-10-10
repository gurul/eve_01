"""claude-mem: eve's memories land in the owner's claude-mem the moment they form, and eve can
recall from it.

claude-mem (github.com/thedotmack/claude-mem, v13.24.23 on this Mac) is the owner's Claude Code memory
plugin. Its worker is a local HTTP service; the port is whatever the plugin chose (37701 today), recorded
in ``~/.claude-mem/settings.json`` (CLAUDE_MEM_WORKER_PORT) and ``~/.claude-mem/worker.pid``.

Three pieces, all off the daemon's loop and none able to raise into it:

* ``ClaudeMemSink`` subscribes to ``/eve/memory/*`` on the MemoryBus and POSTs each event to
  ``/api/memory/save`` from one worker thread through a bounded queue. Project "eve", so a Claude
  Code session searching claude-mem sees what eve saw and taught. What was SAID is not on the bus
  (owner, 2026-09-23): it stays in eve's own memory folder and never reaches claude-mem.
* ``recall(query)`` asks ``GET /api/search`` and returns a short list, newest first, empty on any error.
* ``ClaudeMemMirror`` reads the worker's ``GET /stream`` (server-sent events) and republishes every
  ``new_observation`` from the owner's other projects on ``/claude/observation``, so a rosbridge client
  watching eve also sees the owner's Claude Code work land. eve's own project is never mirrored back.

Verified routes (source: src/services/worker/http/routes/*.ts at v13.24.23):
  POST /api/memory/save   {text, title?, project?, metadata?} -> {"success": true, "id": N, ...}
  GET  /api/search?query=&project=&limit=   -> {"content": [{"type": "text", "text": <markdown table>}]}
  GET  /api/observation/<id>               -> the observation as JSON (title, narrative, created_at, ...)
  GET  /stream                              -> text/event-stream; "new_observation" carries {observation}
  GET  /health                              -> {"status": "ok", ...}

Privacy: the bus never carries the learner's own words (memory_bus.py), and this module adds nothing.
The words "ideas" and "spoken" must not appear in any payload this file builds.
"""

from __future__ import annotations

import contextlib
import json
import logging
import os
import queue
import re
import socket
import threading
import time
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Optional

log = logging.getLogger(__name__)

DEFAULT_PORT = 37777
SOURCE = "eve"
MIRROR_TOPIC = "/claude/observation"
SINK_TOPICS = "/eve/memory/*"
# Lesson actions worth a memory. hint/status/open/listen are chatter; the rest mark progress.
LESSON_ACTIONS_KEPT = frozenset({"start", "step", "check", "recap", "end", "complete"})
_MAX_STR = 2000
_MAX_LIST = 20
_WARN_EVERY_SECS = 600.0
_ROW = re.compile(r"^\|\s*#(\d+)\s*\|\s*([^|]*?)\s*\|\s*[^|]*\|\s*([^|]*?)\s*\|")


# ---- where the worker is ----------------------------------------------------------------

def worker_url(environ: Any = None, home: Optional[Path] = None) -> str:
    """EVE_CLAUDE_MEM_URL wins; else the plugin's settings.json port; else worker.pid; else 37777."""
    env = os.environ if environ is None else environ
    explicit = (env.get("EVE_CLAUDE_MEM_URL") or "").strip()
    if explicit:
        return explicit.rstrip("/")
    base = (home or Path.home()) / ".claude-mem"
    for path, key in ((base / "settings.json", "CLAUDE_MEM_WORKER_PORT"), (base / "worker.pid", "port")):
        try:
            port = int(str(json.loads(path.read_text(encoding="utf-8")).get(key, "")).strip())
        except (OSError, ValueError, TypeError, AttributeError):
            continue
        if 0 < port < 65536:
            return f"http://127.0.0.1:{port}"
    return f"http://127.0.0.1:{DEFAULT_PORT}"


# ---- one bus event -> one memory ---------------------------------------------------------

def _clip(value: str) -> str:
    return value if len(value) <= _MAX_STR else value[:_MAX_STR - 1] + "…"


def _title(prefix: str, text: str) -> str:
    head = " ".join(text.split())[:60]
    return f"{prefix}{head}" if head else prefix.rstrip(": ")


# The only msg fields that ride along as metadata. An allowlist, not "every scalar": a producer that
# one day adds a field with the learner's words in it must not leak it here by accident.
_META_FIELDS = frozenset({
    "id", "lesson_id", "action", "stage", "mode", "topic", "level",        # lessons
    "importance", "novelty", "written", "tags", "changed",                 # diary
    "state", "source",
})


def _metadata(topic: str, msg: dict[str, Any]) -> dict[str, Any]:
    meta: dict[str, Any] = {"platformSource": SOURCE, "topic": topic, "time": msg.get("time")}
    for key in _META_FIELDS:
        if key not in msg or key == "topic":
            continue
        value = msg[key]
        if isinstance(value, bool) or value is None or isinstance(value, (int, float)):
            meta[key] = value
        elif isinstance(value, str):
            meta[key] = _clip(value)
        elif isinstance(value, list) and len(value) <= _MAX_LIST and all(isinstance(v, (str, int, float)) for v in value):
            meta[key] = [_clip(v) if isinstance(v, str) else v for v in value]
    if "topic" in msg and topic == "/eve/memory/lesson":
        meta["subject"] = _clip(str(msg["topic"]))
    return meta


def to_memory(topic: str, msg: dict[str, Any], project: str = "eve") -> Optional[dict[str, Any]]:
    """The /api/memory/save body for one bus event, or None when the event is not worth keeping."""
    if not isinstance(msg, dict):
        return None
    if topic == "/eve/memory/observation":
        thought = str(msg.get("thought") or msg.get("text") or "").strip()
        seen = [str(o) for o in (msg.get("observations") or []) if str(o).strip()][:_MAX_LIST]
        if not thought and not seen:
            return None
        text = _clip(thought) + ("\n\nSeen: " + "; ".join(_clip(s) for s in seen) if seen else "")
        title = _title("eve saw: ", thought or seen[0])
    elif topic == "/eve/memory/lesson":
        action = str(msg.get("action") or "").strip().lower()
        if action not in LESSON_ACTIONS_KEPT:
            return None
        subject = str(msg.get("topic") or "a lesson").strip()
        level = str(msg.get("level") or "").strip()
        feedback = str(msg.get("feedback") or "").strip()
        text = f"{action} on {subject}" + (f" ({level})" if level else "") + (f": {feedback}" if feedback else "")
        title = _title("lesson: ", subject)
    elif topic == "/eve/memory/remember":
        text = str(msg.get("text") or "").strip()
        if not text:
            return None
        title = str(msg.get("title") or "").strip() or _title("", text)
    else:
        return None
    return {"text": text if len(text) <= _MAX_STR * 2 else text[:_MAX_STR * 2 - 1] + "…",
            "title": _clip(title), "project": project, "metadata": _metadata(topic, msg)}


# ---- HTTP, kept small ---------------------------------------------------------------------

def _post(url: str, body: dict[str, Any], timeout: float) -> dict[str, Any]:
    req = urllib.request.Request(url, json.dumps(body).encode("utf-8"),
                                 {"Content-Type": "application/json"}, method="POST")
    with urllib.request.urlopen(req, timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8") or "{}")


def _get(url: str, timeout: float) -> Any:
    with urllib.request.urlopen(urllib.request.Request(url), timeout=timeout) as resp:
        return json.loads(resp.read().decode("utf-8") or "null")


# ---- the sink -------------------------------------------------------------------------------

class ClaudeMemSink:
    """Bus events -> POST /api/memory/save, from one thread, never blocking the publisher."""

    def __init__(self, bus: Any, project: str = "eve", url: Optional[str] = None,
                 queue_size: int = 256, timeout: float = 5.0) -> None:
        self.bus = bus
        self.project = project
        self.url = (url or worker_url()).rstrip("/")
        self.timeout = timeout
        self.stats: dict[str, int] = {"saved": 0, "dropped": 0, "failed": 0}
        self._queue: "queue.Queue[Optional[dict[str, Any]]]" = queue.Queue(maxsize=queue_size)
        self._thread: Optional[threading.Thread] = None
        self._sub: Any = None
        self._last_warned = float("-inf")

    def start(self) -> None:
        if self._thread is not None:
            return
        self._sub = self.bus.subscribe(SINK_TOPICS, self._on_event)
        self._thread = threading.Thread(target=self._run, name="claude-mem-sink", daemon=True)
        self._thread.start()

    def stop(self, wait: float = 2.0) -> None:
        if self._sub is not None:
            self.bus.unsubscribe(self._sub)
            self._sub = None
        thread, self._thread = self._thread, None
        if thread is None:
            return
        try:
            self._queue.put_nowait(None)
        except queue.Full:
            pass
        thread.join(wait)

    # bus thread: enqueue only
    def _on_event(self, topic: str, msg: dict[str, Any]) -> None:
        body = to_memory(topic, msg, self.project)
        if body is None:
            return
        try:
            self._queue.put_nowait(body)
        except queue.Full:
            self.stats["dropped"] += 1

    # worker thread
    def _run(self) -> None:
        while True:
            body = self._queue.get()
            if body is None:
                return
            try:
                _post(self.url + "/api/memory/save", body, self.timeout)
                self.stats["saved"] += 1
            except Exception as e:  # noqa: BLE001 — a dead worker must never take the daemon down
                self.stats["failed"] += 1
                now = time.monotonic()
                if now - self._last_warned >= _WARN_EVERY_SECS:
                    self._last_warned = now
                    log.warning("claude-mem: could not save a memory at %s (%s: %s); further failures are "
                                "counted, not logged, for %d minutes", self.url, type(e).__name__, e,
                                int(_WARN_EVERY_SECS // 60))


# ---- recall -----------------------------------------------------------------------------------

def _parse_rows(markdown: str) -> list[dict[str, Any]]:
    rows = []
    for line in markdown.splitlines():
        m = _ROW.match(line.strip())
        if m:
            rows.append({"id": int(m.group(1)), "time": m.group(2).strip(), "title": m.group(3).strip()})
    return rows


def recall(query: str, limit: int = 5, url: Optional[str] = None, timeout: float = 5.0,
           project: str = "eve") -> list[dict[str, Any]]:
    """Search claude-mem. [{"id", "title", "time", "text"}] newest first; [] on any error."""
    base = (url or worker_url()).rstrip("/")
    limit = max(1, min(int(limit or 5), 20))
    try:
        params = urllib.parse.urlencode({"query": query, "project": project, "limit": limit})
        data = _get(f"{base}/api/search?{params}", timeout)
        text = "\n".join(str(part.get("text") or "") for part in (data or {}).get("content", [])
                         if isinstance(part, dict))
        rows = _parse_rows(text)[:limit]
        results = []
        for row in rows:
            body = ""
            try:
                obs = _get(f"{base}/api/observation/{row['id']}", timeout)
                if isinstance(obs, dict):
                    body = str(obs.get("narrative") or obs.get("text") or "")
                    row["time"] = str(obs.get("created_at") or row["time"])
                    row["title"] = str(obs.get("title") or row["title"])
            except Exception as e:  # noqa: BLE001 — the row still counts without its body
                log.debug("claude-mem: observation %s body unavailable: %s", row["id"], e)
            results.append({"id": row["id"], "title": row["title"], "time": row["time"], "text": _clip(body)})
        return results
    except Exception as e:  # noqa: BLE001
        log.debug("claude-mem: recall failed at %s: %s: %s", base, type(e).__name__, e)
        return []


# ---- the mirror ---------------------------------------------------------------------------

# How long the stream may stay quiet before the mirror reconnects. A timed-out read can never be retried:
# Python marks the socket "timed out" and every later read fails at once ("cannot read from timed out
# object"). The loop used to treat that as another quiet second and retry, so after the first quiet second
# it spun about two million times a second holding the GIL, starving the daemon's event loop (Telegram
# replies lagged, 2026-09-24) and never reading another event. A quiet stream now means reconnect.
IDLE_SECS = 30.0


class ClaudeMemMirror:
    """GET /stream -> /claude/observation for every new observation from the owner's other projects."""

    def __init__(self, bus: Any, url: Optional[str] = None, reconnect_secs: float = 5.0,
                 skip_project: str = "eve", read_timeout: float = IDLE_SECS) -> None:
        self.bus = bus
        self.url = (url or worker_url()).rstrip("/")
        self.reconnect_secs = reconnect_secs
        self.skip_project = skip_project
        self.read_timeout = read_timeout
        self.stats: dict[str, int] = {"mirrored": 0, "reconnects": 0}
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self._last_warned = float("-inf")
        self._live: Any = None                            # the open stream, so stop() can end a blocked read

    def start(self) -> None:
        if self._thread is not None:
            return
        self._stop.clear()
        self._thread = threading.Thread(target=self._run, name="claude-mem-mirror", daemon=True)
        self._thread.start()

    def stop(self, wait: float = 2.0) -> None:
        self._stop.set()
        # A read blocks up to read_timeout: shut the socket down so it returns now.
        sock = getattr(getattr(getattr(self._live, "fp", None), "raw", None), "_sock", None)
        if sock is not None:
            with contextlib.suppress(OSError):
                sock.shutdown(socket.SHUT_RDWR)
        thread, self._thread = self._thread, None
        if thread is not None:
            thread.join(wait)

    def _run(self) -> None:
        first, quiet = True, False
        while not self._stop.is_set():
            if not first and not quiet:                  # a quiet stream reconnects at once, uncounted
                self.stats["reconnects"] += 1
                if self._stop.wait(self.reconnect_secs):
                    return
            first, quiet = False, False
            try:
                quiet = self._read_stream()
            except Exception as e:  # noqa: BLE001 — reconnect, never raise
                now = time.monotonic()
                if now - self._last_warned >= _WARN_EVERY_SECS:
                    self._last_warned = now
                    log.warning("claude-mem: mirror lost %s/stream (%s: %s); retrying every %.0f s",
                                self.url, type(e).__name__, e, self.reconnect_secs)

    def _read_stream(self) -> bool:
        """Read the stream until it ends (False) or stays quiet for read_timeout (True: reconnect at once)."""
        req = urllib.request.Request(self.url + "/stream", headers={"Accept": "text/event-stream"})
        with urllib.request.urlopen(req, timeout=self.read_timeout) as resp:
            self._live = resp
            try:
                while not self._stop.is_set():
                    try:
                        line = resp.readline()
                    except TimeoutError:
                        return True                   # quiet: this socket can never be read again (IDLE_SECS)
                    if not line:
                        return False                  # the worker closed the stream (or stop() shut it)
                    self._on_line(line.decode("utf-8", "replace").rstrip("\r\n"))
            finally:
                self._live = None
        return False

    def _on_line(self, line: str) -> None:
        if not line.startswith("data:"):
            return
        try:
            event = json.loads(line[5:].strip() or "{}")
        except ValueError:
            return
        if not isinstance(event, dict) or event.get("type") != "new_observation":
            return
        obs = event.get("observation")
        if not isinstance(obs, dict):
            return
        project = str(obs.get("project") or "")
        if project == self.skip_project or str(obs.get("platform_source") or "") == SOURCE:
            return
        msg = {"id": obs.get("id"), "title": str(obs.get("title") or ""), "project": project,
               "type": str(obs.get("type") or ""), "time": obs.get("created_at_epoch"),
               "text": _clip(str(obs.get("narrative") or obs.get("text") or ""))}
        self.bus.publish(MIRROR_TOPIC, msg)
        self.stats["mirrored"] += 1
