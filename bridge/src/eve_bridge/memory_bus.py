"""MemoryBus: eve's memory, as it forms, on an in-process pub/sub bus.

eve keeps two memories that go on this bus: the diary (what it sees) is a JSONL stream,
and a lesson is a SQLite row. This bus is the one place those become events the moment they
happen, so that anything else can watch: the rosbridge server (rosbridge.py) fans them out
to WebSocket clients as ROS-style topics, and the claude-mem client (claude_mem.py) stores
them in the owner's claude-mem so a Claude Code session can search eve's day.

Pure Python, stdlib only. daemon.py owns the one instance and binds it to its loop. Publish
is safe from any thread: once the bus is bound, subscribers run on the loop thread; before
that, they run inline. A subscriber that raises is logged and kept.

Topics carry a ROS-style type name so a rosbridge client can `subscribe` with `type`.
What is NOT on the bus, by design: anything said in a conversation (owner, 2026-09-23: the
transcripts and records stay in the memory folder, memory.py), the learner's own words in a
lesson (spoken ideas and typed ideas stay in the lesson store, docs/learning.md § Privacy), and
raw audio or frames. An inbound ``/eve/memory/remember`` line becomes one transcript line
(channel "bus") for the nightly dream to judge — never a star.
"""

from __future__ import annotations

import asyncio
import collections
import logging
import os
import threading
import time
from dataclasses import dataclass, field
from typing import Any, Awaitable, Callable, Deque, Optional

log = logging.getLogger(__name__)

DEFAULT_ROSBRIDGE_HOST = "127.0.0.1"
DEFAULT_ROSBRIDGE_PORT = 9090
_ON = ("1", "true", "yes", "on")


@dataclass(frozen=True)
class BusConfig:
    """Which sinks the daemon attaches to the bus. Everything is off unless asked for."""

    rosbridge: bool = False
    rosbridge_host: str = DEFAULT_ROSBRIDGE_HOST
    rosbridge_port: int = DEFAULT_ROSBRIDGE_PORT
    claude_mem: bool = False
    claude_mem_url: Optional[str] = None
    mirror: bool = False


def configured(environ: Any = None) -> BusConfig:
    """EVE_ROSBRIDGE / EVE_ROSBRIDGE_HOST / EVE_ROSBRIDGE_PORT,
    EVE_CLAUDE_MEM / EVE_CLAUDE_MEM_URL / EVE_CLAUDE_MEM_MIRROR (defaults to CLAUDE_MEM)."""
    env = os.environ if environ is None else environ

    def flag(name: str, default: bool = False) -> bool:
        raw = (env.get(name) or "").strip().lower()
        return default if not raw else raw in _ON

    port = DEFAULT_ROSBRIDGE_PORT
    raw_port = (env.get("EVE_ROSBRIDGE_PORT") or "").strip()
    if raw_port:
        try:
            port = int(raw_port)
        except ValueError:
            log.warning("memory bus: EVE_ROSBRIDGE_PORT=%r is not a number; using %d", raw_port, port)
    claude_mem = flag("EVE_CLAUDE_MEM")
    return BusConfig(
        rosbridge=flag("EVE_ROSBRIDGE"),
        rosbridge_host=(env.get("EVE_ROSBRIDGE_HOST") or "").strip() or DEFAULT_ROSBRIDGE_HOST,
        rosbridge_port=port,
        claude_mem=claude_mem,
        claude_mem_url=(env.get("EVE_CLAUDE_MEM_URL") or "").strip() or None,
        mirror=flag("EVE_CLAUDE_MEM_MIRROR", default=claude_mem),
    )

# topic -> type. The msg shapes are documented in docs/memory-bus.md.
TOPICS: dict[str, str] = {
    "/eve/memory/observation": "eve_msgs/Observation",      # diary: a written thought about the room
    "/eve/memory/lesson": "eve_msgs/LessonEvent",           # a lesson action and eve's feedback
    "/eve/memory/remember": "eve_msgs/Remember",            # inbound: someone asks eve to keep a line
    "/eve/state": "eve_msgs/AgentState",                    # idle / wake / listening / thinking / speaking
    "/eve/presence": "eve_msgs/Presence",                   # follow.py: seen / lost / found (and how), with the angles
    "/claude/observation": "eve_msgs/ClaudeObservation",      # mirrored from the owner's claude-mem stream
}
# service -> type. args and values shapes are in docs/memory-bus.md.
SERVICES: dict[str, str] = {
    "/eve/memory/recall": "eve_msgs/Recall",                # {"query": str, "limit": int} -> {"results": [...]}
}
RECENT_PER_TOPIC = 20

Callback = Callable[[str, dict[str, Any]], Any]
ServiceHandler = Callable[[dict[str, Any]], Awaitable[dict[str, Any]]]


@dataclass(frozen=True)
class Event:
    topic: str
    msg: dict[str, Any]
    time: float


@dataclass
class Subscription:
    topic: str
    callback: Callback
    id: int = field(default=0)


class MemoryBus:
    def __init__(self, clock: Callable[[], float] = time.time) -> None:
        self._subs: dict[str, list[Subscription]] = {}
        self._services: dict[str, ServiceHandler] = {}
        self._recent: dict[str, Deque[Event]] = {}
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._lock = threading.Lock()
        self._next_id = 1
        self._clock = clock
        self.dropped = 0

    # -- wiring --
    def bind_loop(self, loop: asyncio.AbstractEventLoop) -> None:
        """After this, callbacks run on `loop`'s thread even when publish() is called elsewhere."""
        self._loop = loop

    def subscribe(self, topic: str, callback: Callback) -> Subscription:
        """Subscribe to one topic, or to a prefix ending in '*' ("/eve/memory/*")."""
        with self._lock:
            sub = Subscription(topic, callback, self._next_id)
            self._next_id += 1
            self._subs.setdefault(topic, []).append(sub)
        return sub

    def unsubscribe(self, sub: Subscription) -> None:
        with self._lock:
            subs = self._subs.get(sub.topic, [])
            self._subs[sub.topic] = [s for s in subs if s.id != sub.id]

    def register_service(self, name: str, handler: ServiceHandler) -> None:
        self._services[name] = handler

    def has_service(self, name: str) -> bool:
        return name in self._services

    async def call_service(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        handler = self._services.get(name)
        if handler is None:
            raise KeyError(f"no such service: {name}")
        return await handler(dict(args or {}))

    # -- events --
    def publish(self, topic: str, msg: dict[str, Any]) -> Event:
        """Deliver `msg` on `topic`. Safe from any thread. Never raises for a subscriber's error."""
        event = Event(topic, dict(msg), self._clock())
        with self._lock:
            self._recent.setdefault(topic, collections.deque(maxlen=RECENT_PER_TOPIC)).append(event)
            targets = list(self._subs.get(topic, []))
            for pattern, subs in self._subs.items():
                if pattern.endswith("*") and pattern != topic and topic.startswith(pattern[:-1]):
                    targets.extend(subs)
        loop = self._loop
        if loop is not None and not loop.is_closed():
            try:
                running = asyncio.get_running_loop()
            except RuntimeError:
                running = None
            if running is loop:
                self._deliver(targets, event)
            else:
                loop.call_soon_threadsafe(self._deliver, targets, event)
        else:
            self._deliver(targets, event)
        return event

    def _deliver(self, targets: list[Subscription], event: Event) -> None:
        for sub in targets:
            try:
                result = sub.callback(event.topic, event.msg)
                if asyncio.iscoroutine(result):
                    loop = self._loop
                    if loop is not None and not loop.is_closed():
                        loop.create_task(result)
                    else:
                        result.close()
                        self.dropped += 1
            except Exception:  # noqa: BLE001
                log.exception("memory bus: subscriber failed on %s", event.topic)

    def recent(self, topic: str, n: int = RECENT_PER_TOPIC) -> list[Event]:
        with self._lock:
            return list(self._recent.get(topic, ()))[-n:]

    def topics(self) -> dict[str, str]:
        return dict(TOPICS)

    def services(self) -> dict[str, str]:
        return dict(SERVICES)
