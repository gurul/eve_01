"""phone_call.PhoneCalls.serve_socket: a call on a socket that is already open, ``serve`` without the WebSocket
handshake (eve lite's WebRTC call, lite_rtc.RtcCallSocket; docs/plans/eve-lite.md, M6 and M11).

It is additive: phone_call.py is the file it was at d419b37 plus this one method, byte for byte (with a positive
control), and a scripted call gives the same messages, audio and effects through ``serve`` over a real WebSocket
and through ``serve_socket`` over an in-memory socket. test_phone_call.py, unmodified, covers ``serve`` itself."""
from __future__ import annotations

import ast
import asyncio
import hashlib
import json
from pathlib import Path
from typing import Any, Optional

from test_phone_call import Brain, Voice
from websockets.asyncio.client import connect

from eve_bridge import phone_call
from eve_bridge.phone_call import OP_BIN, OP_TEXT, Closed, PhoneCalls

TOKEN = "call-token-CANARY-NOT-A-KEY-7f3c"
# phone_call.py at d419b37 (git show d419b37:bridge/src/eve_bridge/phone_call.py | shasum -a 256), the commit eve
# lite branched from. If phone_call.py changes on purpose outside serve_socket, record the new file's hash here.
BASE_SHA256 = "674b3c9ae202e6f8b9ade4de7237eba8021e4d3e936dc2a117d3cfdefa10f958"


def without_serve_socket(text: str) -> str:
    """phone_call.py's text with PhoneCalls.serve_socket and the blank line before it taken out."""
    tree = ast.parse(text)
    cls = next(n for n in tree.body if isinstance(n, ast.ClassDef) and n.name == "PhoneCalls")
    fn = next(n for n in cls.body if isinstance(n, ast.AsyncFunctionDef) and n.name == "serve_socket")
    lines = text.split("\n")
    first, last = fn.lineno - 1, fn.end_lineno or fn.lineno                         # 0-based start, 1-based end
    assert not fn.decorator_list and lines[first - 1] == ""
    return "\n".join(lines[:first - 1] + lines[last:])


def sha256(text: str) -> str:
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def test_serve_socket_is_the_only_change_to_phone_call_since_the_base() -> None:
    text = Path(phone_call.__file__).read_bytes().decode("utf-8")
    got = sha256(without_serve_socket(text))
    assert got == BASE_SHA256, f"phone_call.py changed outside serve_socket since d419b37 (now {got})"
    # positive control: one character anywhere else is caught, and so is anything added beside the method
    assert sha256(without_serve_socket(text.replace("AUTH_SECS = 10.0", "AUTH_SECS = 11.0", 1))) != BASE_SHA256
    added = text.replace("    async def _serve(self,", "    async def extra(self) -> None:\n        pass\n\n"
                                                        "    async def _serve(self,", 1)
    assert sha256(without_serve_socket(added)) != BASE_SHA256


# ---- an in-memory socket -----------------------------------------------------------------------------------------

class MemorySocket:
    """What serve_socket drives: ``recv``, ``send_json``, ``send_bytes``, ``close`` and ``closed``. ``page`` feeds it
    a message; ``out`` is everything the call sent, in order (JSON as dicts, audio as bytes)."""

    def __init__(self) -> None:
        self.inbox: asyncio.Queue[Optional[tuple[int, bytes]]] = asyncio.Queue()
        self.out: asyncio.Queue[Any] = asyncio.Queue()
        self.sent: list[Any] = []
        self.closed = False
        self.closes = 0

    def page(self, msg: Any) -> None:
        if isinstance(msg, bytes):
            self.inbox.put_nowait((OP_BIN, msg))
        else:
            self.inbox.put_nowait((OP_TEXT, (msg if isinstance(msg, str) else json.dumps(msg)).encode()))

    async def recv(self) -> tuple[int, bytes]:
        item = await self.inbox.get()
        if item is None:
            raise Closed("closed")
        return item

    async def send_json(self, obj: dict[str, Any]) -> None:
        if self.closed:
            raise Closed("closed")
        self.sent.append(obj)
        self.out.put_nowait(obj)

    async def send_bytes(self, data: bytes) -> None:
        if self.closed:
            raise Closed("closed")
        self.sent.append(bytes(data))
        self.out.put_nowait(bytes(data))

    async def close(self, code: int = 1000) -> None:
        self.closes += 1
        self.closed = True
        self.inbox.put_nowait(None)                                                 # its own close ends its recv


async def script(send: Any, recv: Any) -> list[Any]:
    """One call as the page makes it: the hello, a one-second press, the answer heard out, then the end. → what
    the call sent, JSON as dicts and each audio message as its length."""
    got: list[Any] = []

    async def until(kind: str, **fields: Any) -> None:
        async with asyncio.timeout(5):
            while True:
                m = await recv()
                got.append(m if isinstance(m, dict) else len(m))
                if isinstance(m, dict) and m.get("type") == kind and all(m.get(k) == v for k, v in fields.items()):
                    return

    await send({"token": TOKEN, "can": []})
    await until("state", state="connected")
    await send({"type": "talk"})
    for _ in range(10):
        await send(bytes(4800))
    await send({"type": "done"})
    await until("state", state="listening")
    await send({"type": "end"})
    await until("ended")
    return got


def test_a_call_on_an_open_socket_runs_as_serve_runs_it_without_the_handshake() -> None:
    async def go() -> tuple[list[Any], list[Any], tuple[Any, ...], tuple[Any, ...]]:
        # through serve: a real WebSocket, its handshake and frames
        brain, voice = Brain(), Voice()
        calls = PhoneCalls(lambda _: None, brain, voice)

        async def handle(reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
            head = (await reader.readuntil(b"\r\n\r\n")).decode().split("\r\n")
            headers = {k.strip().lower(): v.strip() for k, _, v in (ln.partition(":") for ln in head[1:] if ln)}
            await calls.serve(reader, writer, headers, token_check=lambda t: t == TOKEN)
            writer.close()

        server = await asyncio.start_server(handle, "127.0.0.1", 0)
        port = server.sockets[0].getsockname()[1]
        async with connect(f"ws://127.0.0.1:{port}/api/call") as ws:
            async def ws_send(m: Any) -> None:
                await ws.send(m if isinstance(m, bytes) else json.dumps(m))

            async def ws_recv() -> Any:
                m = await ws.recv()
                return m if isinstance(m, bytes) else json.loads(m)

            over_ws = await script(ws_send, ws_recv)
        server.close()
        await server.wait_closed()
        ws_effects = (brain.heard, voice.transcribed, voice.spoken, brain.say, calls.active)

        # through serve_socket: the same call on a socket that is already open
        brain, voice = Brain(), Voice()
        calls = PhoneCalls(lambda _: None, brain, voice)
        sock = MemorySocket()
        task = asyncio.ensure_future(calls.serve_socket(sock, lambda t: t == TOKEN))

        async def mem_send(m: Any) -> None:
            sock.page(m)

        over_socket = await script(mem_send, sock.out.get)
        await asyncio.wait_for(task, 5)
        assert sock.closed and sock.closes >= 1                                     # closed when the call ends
        socket_effects = (brain.heard, voice.transcribed, voice.spoken, brain.say, calls.active)
        return over_ws, over_socket, ws_effects, socket_effects

    over_ws, over_socket, ws_effects, socket_effects = asyncio.run(go())
    assert over_socket == over_ws
    kinds = [m["type"] if isinstance(m, dict) else "audio" for m in over_socket]
    assert kinds[:4] == ["state", "flush", "state", "heard"] and kinds[-1] == "ended" and "audio" in kinds
    assert socket_effects == ws_effects
    assert socket_effects[0] == ["what's on my calendar"] and socket_effects[1] == [48000]
    assert socket_effects[3] is None and socket_effects[4] is None                  # the chat let go; no call left
    assert {"type": "ended", "reason": phone_call.HUNG_UP} in over_socket


def test_serve_socket_checks_the_hello_and_says_why_it_ends() -> None:
    async def run(hello: Any, token_check: Any, brain: Any = None, voice: Any = None) -> MemorySocket:
        calls = PhoneCalls(lambda _: None, brain, voice)
        sock = MemorySocket()
        if hello is not None:
            sock.page(hello)
        await asyncio.wait_for(calls.serve_socket(sock, token_check), 5)
        return sock

    async def go() -> None:
        check = lambda t: t == TOKEN                                                  # noqa: E731
        for hello in ({"token": "wrong"}, {"token": ""}, {"initData": "x"}, "not json", [TOKEN], b"\x00\x01"):
            sock = await run(hello, check, Brain(), Voice())
            assert sock.sent == [{"type": "ended", "reason": "Only eve's owner can call."}] and sock.closed
        sock = await run({"token": TOKEN, "can": []}, check)                         # no chat or voice here
        assert sock.sent == [{"type": "ended", "reason": "Calls aren't set up on this computer."}]
        # a socket that closes before saying anything: nothing is sent, and it is closed once more, harmlessly
        calls = PhoneCalls(lambda _: None, Brain(), Voice())
        sock = MemorySocket()
        sock.inbox.put_nowait(None)
        await asyncio.wait_for(calls.serve_socket(sock, check), 5)
        assert sock.closed and calls.active is None

    asyncio.run(go())


def test_a_second_socket_replaces_the_call_still_open() -> None:
    async def go() -> None:
        brain = Brain()
        calls = PhoneCalls(lambda _: None, brain, Voice())
        first, second = MemorySocket(), MemorySocket()
        first.page({"token": TOKEN, "can": []})
        one = asyncio.ensure_future(calls.serve_socket(first, lambda t: t == TOKEN))
        await asyncio.wait_for(first.out.get(), 5)
        assert calls.active is not None and calls.active.ws is first
        second.page({"token": TOKEN, "can": []})
        two = asyncio.ensure_future(calls.serve_socket(second, lambda t: t == TOKEN))
        assert await asyncio.wait_for(second.out.get(), 5) == {"type": "state", "state": "connected"}
        assert first.closed and calls.active is not None and calls.active.ws is second
        await asyncio.wait_for(one, 5)
        second.page({"type": "end"})
        await asyncio.wait_for(two, 5)
        assert calls.active is None and second.sent[-1] == {"type": "ended", "reason": phone_call.HUNG_UP}

    asyncio.run(go())
