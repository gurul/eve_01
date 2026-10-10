"""A rosbridge v2 server over the MemoryBus: eve's memory as ROS-style topics on a WebSocket.

Any rosbridge client (roslibjs, roslibpy, Foxglove) connects to ``ws://127.0.0.1:9090`` and:

- ``subscribe`` to a topic from memory_bus.TOPICS (or a prefix ending in ``*``) and receive
  every bus event as ``{"op": "publish", "topic": ..., "msg": ...}``;
- ``publish`` to a known topic (``/eve/memory/remember``) and the message enters the bus;
- ``call_service`` on a bus service (``/eve/memory/recall``) and get a ``service_response``.

The daemon never waits on a client. Each client owns a bounded outbound queue and a writer
task: when the queue is full the oldest queued message is dropped and counted, logged once
per client. A client that disconnects takes its bus subscriptions with it.

Protocol reference: rosbridge_suite ROSBRIDGE_PROTOCOL.md (ops advertise, unadvertise,
publish, subscribe, unsubscribe, call_service, service_response, status, set_level).
Compression and fragmentation are not implemented; ``fragment_size``/``compression`` are ignored.

There is no path here that reads a lesson store: the bus already carries no learner words.

**No browser pages.** A handshake that carries an ``Origin`` header comes from a web page, and any page the owner
opens could otherwise reach 127.0.0.1 and read or write eve's memory (v3 safety review): it is refused (HTTP 403).
Desktop clients (roslibpy, Foxglove's desktop app, a script) send no Origin and connect as before.
"""

from __future__ import annotations

import asyncio
import json
import logging
import time
from dataclasses import dataclass, field
from typing import Any, Optional

from websockets.asyncio.server import Server, ServerConnection, serve
from websockets.exceptions import ConnectionClosed

from .memory_bus import MemoryBus, Subscription

log = logging.getLogger(__name__)

DEFAULT_HOST = "127.0.0.1"
DEFAULT_PORT = 9090
DEFAULT_QUEUE_SIZE = 64
NO_BROWSER_ORIGINS: tuple[Optional[str], ...] = (None,)   # only a handshake with no Origin header is accepted


@dataclass
class _Sub:
    """One client subscription: the bus subscription plus the rosbridge bookkeeping."""
    topic: str
    sub_id: Optional[str]
    bus_sub: Subscription
    throttle_secs: float = 0.0
    last_sent: float = field(default=float("-inf"))


class _Client:
    def __init__(self, conn: ServerConnection, queue_size: int, clock) -> None:
        self.conn = conn
        self.queue: asyncio.Queue[str] = asyncio.Queue(maxsize=queue_size)
        self.subs: list[_Sub] = []
        self.advertised: set[str] = set()
        self.writer: Optional[asyncio.Task] = None
        self.tasks: set[asyncio.Task] = set()
        self.dropped = 0
        self.warned = False
        self._clock = clock

    @property
    def name(self) -> str:
        peer = getattr(self.conn, "remote_address", None)
        return f"{peer[0]}:{peer[1]}" if isinstance(peer, tuple) and len(peer) >= 2 else "client"


class RosbridgeServer:
    def __init__(self, bus: MemoryBus, host: str = DEFAULT_HOST, port: int = DEFAULT_PORT,
                 queue_size: int = DEFAULT_QUEUE_SIZE, clock=time.monotonic) -> None:
        self.bus = bus
        self.origins = NO_BROWSER_ORIGINS
        self.host = host
        self.port = port
        self.queue_size = queue_size
        self._clock = clock
        self._server: Optional[Server] = None
        self._clients: set[_Client] = set()
        self.stats: dict[str, int] = {"sent": 0, "dropped": 0, "clients_total": 0}

    # -- lifecycle --
    async def start(self) -> None:
        if self._server is not None:
            return
        self._server = await serve(self._handle, self.host, self.port, ping_interval=20, ping_timeout=20,
                                   max_size=1 << 20, origins=list(self.origins))
        # port 0 asks the OS for a free one; record what it gave us.
        for sock in self._server.sockets or ():
            self.port = sock.getsockname()[1]
            break
        log.info("rosbridge: serving eve's memory at %s", self.url)

    async def stop(self) -> None:
        server, self._server = self._server, None
        if server is None:
            return
        for client in list(self._clients):
            await self._close_client(client)
        server.close()
        await server.wait_closed()
        log.info("rosbridge: stopped (%d sent, %d dropped, %d clients over its life)",
                 self.stats["sent"], self.stats["dropped"], self.stats["clients_total"])

    @property
    def url(self) -> str:
        return f"ws://{self.host}:{self.port}"

    # -- one client --
    async def _handle(self, conn: ServerConnection) -> None:
        client = _Client(conn, self.queue_size, self._clock)
        self._clients.add(client)
        self.stats["clients_total"] += 1
        client.writer = asyncio.create_task(self._write_loop(client), name="rosbridge-writer")
        try:
            async for raw in conn:
                self._on_message(client, raw)
        except ConnectionClosed:
            pass
        finally:
            await self._close_client(client)

    async def _close_client(self, client: _Client) -> None:
        if client not in self._clients:
            return
        self._clients.discard(client)
        for sub in client.subs:
            self.bus.unsubscribe(sub.bus_sub)
        client.subs.clear()
        for task in list(client.tasks):
            task.cancel()
        if client.writer is not None:
            client.writer.cancel()
            await asyncio.gather(client.writer, return_exceptions=True)
        try:
            await client.conn.close()
        except Exception:  # noqa: BLE001
            pass

    async def _write_loop(self, client: _Client) -> None:
        while True:
            text = await client.queue.get()
            try:
                await client.conn.send(text)
                self.stats["sent"] += 1
            except ConnectionClosed:
                return

    def _enqueue(self, client: _Client, message: dict[str, Any]) -> None:
        """Queue one outbound message. Full queue: drop the oldest, never block the caller."""
        text = json.dumps(message, ensure_ascii=False, default=str)
        while True:
            try:
                client.queue.put_nowait(text)
                return
            except asyncio.QueueFull:
                try:
                    client.queue.get_nowait()
                except asyncio.QueueEmpty:
                    continue
                client.dropped += 1
                self.stats["dropped"] += 1
                if not client.warned:
                    client.warned = True
                    log.warning("rosbridge: %s is not keeping up; dropping its oldest messages", client.name)

    def _status(self, client: _Client, level: str, msg: str, msg_id: Optional[str] = None) -> None:
        out: dict[str, Any] = {"op": "status", "level": level, "msg": msg}
        if msg_id is not None:
            out["id"] = msg_id
        self._enqueue(client, out)

    # -- inbound ops --
    def _on_message(self, client: _Client, raw: Any) -> None:
        try:
            data = json.loads(raw)
        except (ValueError, TypeError):
            self._status(client, "error", "message is not valid JSON")
            return
        if not isinstance(data, dict):
            self._status(client, "error", "message must be a JSON object")
            return
        op = data.get("op")
        msg_id = data.get("id") if isinstance(data.get("id"), (str, int)) else None
        msg_id = str(msg_id) if msg_id is not None else None
        handler = {
            "subscribe": self._op_subscribe, "unsubscribe": self._op_unsubscribe,
            "advertise": self._op_advertise, "unadvertise": self._op_unadvertise,
            "publish": self._op_publish, "call_service": self._op_call_service,
            "set_level": self._op_set_level,
        }.get(op if isinstance(op, str) else "")
        if handler is None:
            self._status(client, "error", f"unsupported op: {op!r}", msg_id)
            return
        handler(client, data, msg_id)

    def _known_topic(self, topic: str) -> bool:
        if topic in self.bus.topics():
            return True
        if topic.endswith("*"):
            prefix = topic[:-1]
            return any(t.startswith(prefix) for t in self.bus.topics())
        return False

    def _op_subscribe(self, client: _Client, data: dict[str, Any], msg_id: Optional[str]) -> None:
        topic = data.get("topic")
        if not isinstance(topic, str) or not topic:
            self._status(client, "error", "subscribe needs a topic", msg_id)
            return
        throttle = data.get("throttle_rate")
        throttle_secs = max(0.0, float(throttle)) / 1000.0 if isinstance(throttle, (int, float)) else 0.0
        sub = _Sub(topic, msg_id, None, throttle_secs)  # type: ignore[arg-type]

        def on_event(evt_topic: str, msg: dict[str, Any], sub=sub, client=client) -> None:
            now = self._clock()
            if sub.throttle_secs and now - sub.last_sent < sub.throttle_secs:
                return
            sub.last_sent = now
            self._enqueue(client, {"op": "publish", "topic": evt_topic, "msg": msg})

        sub.bus_sub = self.bus.subscribe(topic, on_event)
        client.subs.append(sub)
        if not self._known_topic(topic):
            self._status(client, "error", f"unknown topic {topic}; subscribed anyway", msg_id)

    def _op_unsubscribe(self, client: _Client, data: dict[str, Any], msg_id: Optional[str]) -> None:
        topic = data.get("topic")
        keep: list[_Sub] = []
        for sub in client.subs:
            same_topic = sub.topic == topic
            same_id = msg_id is None or sub.sub_id == msg_id
            if same_topic and same_id:
                self.bus.unsubscribe(sub.bus_sub)
            else:
                keep.append(sub)
        client.subs = keep

    def _op_advertise(self, client: _Client, data: dict[str, Any], msg_id: Optional[str]) -> None:
        topic = data.get("topic")
        if isinstance(topic, str):
            client.advertised.add(topic)

    def _op_unadvertise(self, client: _Client, data: dict[str, Any], msg_id: Optional[str]) -> None:
        client.advertised.discard(data.get("topic"))  # type: ignore[arg-type]

    def _op_set_level(self, client: _Client, data: dict[str, Any], msg_id: Optional[str]) -> None:
        return

    def _op_publish(self, client: _Client, data: dict[str, Any], msg_id: Optional[str]) -> None:
        topic = data.get("topic")
        msg = data.get("msg")
        if not isinstance(topic, str) or topic not in self.bus.topics():
            self._status(client, "error", f"cannot publish to unknown topic {topic!r}", msg_id)
            return
        if not isinstance(msg, dict):
            self._status(client, "error", "publish needs an object msg", msg_id)
            return
        self.bus.publish(topic, msg)

    def _op_call_service(self, client: _Client, data: dict[str, Any], msg_id: Optional[str]) -> None:
        service = data.get("service")
        args = data.get("args")
        if not isinstance(service, str) or not service:
            self._status(client, "error", "call_service needs a service", msg_id)
            return
        if not isinstance(args, dict):
            args = {}
        task = asyncio.create_task(self._call(client, service, args, msg_id), name="rosbridge-service")
        client.tasks.add(task)
        task.add_done_callback(client.tasks.discard)

    async def _call(self, client: _Client, service: str, args: dict[str, Any], msg_id: Optional[str]) -> None:
        out: dict[str, Any] = {"op": "service_response", "service": service}
        if msg_id is not None:
            out["id"] = msg_id
        if not self.bus.has_service(service):
            out.update(result=False, values={"error": "no such service"})
        else:
            try:
                values = await self.bus.call_service(service, args)
                out.update(result=True, values=values if isinstance(values, dict) else {"value": values})
            except asyncio.CancelledError:
                raise
            except Exception as e:  # noqa: BLE001
                log.warning("rosbridge: service %s failed: %s: %s", service, type(e).__name__, e)
                out.update(result=False, values={"error": f"{type(e).__name__}: {e}"})
        self._enqueue(client, out)
