"""lite_rtc.LinkChannel: the web app's ``link`` data channel carrying the unchanged LinkChat (link_chat.py) as HTTP
over messages (docs/plans/eve-lite.md, M6).

The real LinkChat, with test_link_chat.py's fake inlet and a real transcript store in a tmp folder. The channel is
a fake with the surface aiortc's RTCDataChannel gives LinkChannel (send, readyState, bufferedAmount and its low
event, close), so every message the page would get is read back as the page reads it (``answers``): a head, the
body's frames under the request's id, then end or err, in that order and nothing after."""
from __future__ import annotations

import asyncio
import json
import struct
import time
from pathlib import Path
from typing import Any, Callable, Optional

import pytest
from pyee.asyncio import AsyncIOEventEmitter
from test_link_chat import FakeInlet, events, store_at, two_lines

from eve_bridge import link_chat, lite_rtc
from eve_bridge.link_chat import HiddenJobs, LinkChat
from eve_bridge.link_uploads import UploadStore
from eve_bridge.lite_rtc import ChannelWriter, LinkChannel

SECRET = "peer-secret-CANARY-NOT-A-KEY-7f3c"
TURN, MESSAGES = link_chat.TURN_PATH, link_chat.MESSAGES_PATH


# ---- the page's side -----------------------------------------------------------------------------------------

class FakeChannel(AsyncIOEventEmitter):
    """aiortc's RTCDataChannel as lite_rtc uses it. What is sent is kept in order; ``page`` is a message from the
    page; ``close`` closes it the way aiortc does (readyState, then the "close" event)."""

    def __init__(self, label: str = "link", *, ordered: bool = True) -> None:
        super().__init__()
        self.label, self.ordered = label, ordered
        self.maxPacketLifeTime: Optional[int] = None
        self.maxRetransmits: Optional[int] = None
        self.readyState = "open"
        self.bufferedAmount = 0
        self.bufferedAmountLowThreshold = 0
        self.sent: list[Any] = []

    def send(self, data: Any) -> None:
        if self.readyState != "open":
            raise RuntimeError("InvalidStateError")
        self.sent.append(data)

    def close(self) -> None:
        if self.readyState == "open":
            self.readyState = "closed"
            self.emit("close")

    def page(self, msg: Any) -> None:
        self.emit("message", msg if isinstance(msg, (str, bytes)) else json.dumps(msg))


def answers(sent: list[Any]) -> dict[int, dict[str, Any]]:
    """Each request's answer as the page reassembles it: ``head`` (the message), ``body`` (its frames joined),
    ``frames`` and ``end`` ("end" or "err"). The order is checked as it is read."""
    out: dict[int, dict[str, Any]] = {}
    for m in sent:
        if isinstance(m, bytes):
            assert len(m) > 4, "a frame with no body"
            rid = struct.unpack(">I", m[:4])[0]
            assert len(m) - 4 <= lite_rtc.BODY_FRAME_BYTES
            a = out[rid]
            assert a["head"] is not None and a["end"] is None, "a frame outside head..end"
            a["body"] += m[4:]
            a["frames"] += 1
            continue
        msg = json.loads(m)
        a = out.setdefault(msg["id"], {"head": None, "body": b"", "frames": 0, "end": None})
        assert a["end"] is None, "nothing may follow end or err"
        if msg["t"] == "head":
            assert a["head"] is None and set(msg) == {"t", "id", "s", "h"}
            a["head"] = msg
        else:
            assert msg["t"] in ("end", "err") and set(msg) == {"t", "id"}
            a["end"] = msg["t"]
    return out


def req(rid: Any, method: str, path: str, body: Any = None, query: str = "",
        headers: Optional[dict[str, str]] = None) -> dict[str, Any]:
    msg: dict[str, Any] = {"t": "req", "id": rid, "m": method, "p": path, "q": query, "h": headers or {}}
    if body is not None:
        msg["b"] = body if isinstance(body, str) else json.dumps(body)
    return msg


async def until(pred: Callable[[], Any], secs: float = 5.0) -> None:
    end = time.monotonic() + secs
    while not pred():
        assert time.monotonic() < end, "timed out"
        await asyncio.sleep(0.01)


def ended(ch: FakeChannel, rid: int) -> Callable[[], bool]:
    return lambda: bool(answers(ch.sent).get(rid, {}).get("end"))


def door(tmp_path: Path, inlet: Any, **kw: Any) -> LinkChat:
    tmp_path.mkdir(parents=True, exist_ok=True)
    kw.setdefault("settle_secs", 0.01)
    return LinkChat(lambda: inlet, uploads=UploadStore(tmp_path / "uploads"),
                    hidden=HiddenJobs(tmp_path / "hidden-jobs.json"), **kw)


# ---- a turn ----------------------------------------------------------------------------------------------------

def test_a_turn_streams_as_head_frames_and_end_with_the_sse_dechunked(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), two_lines)
        link = door(tmp_path, inlet)
        ch = FakeChannel()
        lc = LinkChannel(ch, link, SECRET)
        ch.page(req(1, "POST", TURN, {"text": "  hello eve  ", "client_id": "rtc-turn-1"},
                    headers={"Content-Type": "application/json"}))
        await until(ended(ch, 1))
        a = answers(ch.sent)[1]
        assert a["end"] == "end" and a["head"]["s"] == 200
        head = a["head"]["h"]
        assert head["content-type"] == "text/event-stream" and head["cache-control"] == "no-store"
        assert "transfer-encoding" not in head and "connection" not in head        # de-chunked here
        contents, done, _ = events(a["body"])                                      # SSE events, well formed
        assert contents == ["First line.", "\n\nSecond line."] and done
        assert a["body"].endswith(b"data: [DONE]\n\n") and b"\r\n" not in a["body"]   # no chunk framing left
        assert inlet.heard == [("hello eve", False, "rtc-turn-1")]                  # typed, the app's turn
        assert inlet.delivered == [True, True]                                     # a reader had both lines
        assert lc.requests == {}                                                   # the slot is free again
        link.close()

    asyncio.run(go())


def test_while_a_turn_streams_it_has_one_reader_and_a_cancel_hangs_up(tmp_path: Path) -> None:
    async def forever(inlet: FakeInlet, app: str) -> None:
        inlet.out(app, "Working on it.")                                           # busy, never done

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), forever)
        link = door(tmp_path, inlet, ping_secs=30.0)
        ch = FakeChannel()
        lc = LinkChannel(ch, link, SECRET)
        ch.page(req(7, "POST", TURN, {"text": "hi", "client_id": "rtc-turn-7"}))
        await until(lambda: b"Working on it." in answers(ch.sent).get(7, {}).get("body", b""))
        turn = link._turns["rtc-turn-7"]
        assert turn.readers == 1 and not turn.done
        reader, task = lc.requests[7].reader, lc.requests[7].task
        assert not reader.at_eof()
        sent = len(ch.sent)
        ch.page({"t": "cancel", "id": 7})
        assert lc.requests == {}                                                   # the slot is free at once
        await until(lambda: turn.readers == 0)
        assert reader.at_eof() and task is not None and task.done()
        inlet.out(turn.id, "Said after the cancel.")                               # the brain is still going
        assert inlet.delivered == [True, False]                                    # and nobody reads it now
        await asyncio.sleep(0.05)
        assert len(ch.sent) == sent                                                # no end, no err, no frames
        ch.page({"t": "cancel", "id": 7})                                          # twice: nothing happens
        ch.page({"t": "cancel", "id": 99})
        assert len(ch.sent) == sent
        link.close()

    asyncio.run(go())


def test_no_eof_reaches_linkchat_before_a_cancel(tmp_path: Path) -> None:
    async def late(inlet: FakeInlet, app: str) -> None:
        await asyncio.sleep(0.3)                                                   # long after the body was read
        inlet.out(app, "Late line.")
        inlet.busy = False

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), late)
        link = door(tmp_path, inlet)
        ch = FakeChannel()
        lc = LinkChannel(ch, link, SECRET)
        ch.page(req(1, "POST", TURN, {"text": "hi", "client_id": "rtc-late-1"}))
        await asyncio.sleep(0.1)
        assert not lc.requests[1].reader.at_eof()                                 # read whole, never ended
        await until(ended(ch, 1))
        assert inlet.delivered == [True]                                           # a reader for the late line
        assert events(answers(ch.sent)[1]["body"])[:2] == (["Late line."], True)

        # Positive control: the same turn read from a reader that ends with its body, as a plain stream would
        # be. LinkChat takes that EOF as the page hanging up, and the late line has no reader.
        inlet2 = FakeInlet(store_at(tmp_path / "eof"), late)
        link2 = door(tmp_path / "eof", inlet2)
        lc2 = LinkChannel(FakeChannel(), link2, SECRET)
        body = json.dumps({"text": "hi", "client_id": "rtc-late-2"}).encode()
        reader = asyncio.StreamReader()
        reader.feed_data(body)
        reader.feed_eof()
        await link2.serve("POST", TURN, "", {"authorization": f"Bearer {SECRET}", "content-length": str(len(body))},
                          reader, ChannelWriter(lc2, 9), lc2.check)
        assert inlet2.delivered == [False]
        link.close()
        link2.close()

    asyncio.run(go())


# ---- the conversation, and what never reaches LinkChat ---------------------------------------------------------

def test_a_long_messages_page_is_split_into_frames_and_reassembled(tmp_path: Path) -> None:
    store = store_at(tmp_path)
    for i in range(link_chat.HISTORY_LIMIT):
        assert store.append("telegram", "t-1", "eve", "say", f"{i:03d} " + "words " * 180)

    async def go() -> None:
        link = door(tmp_path, FakeInlet(store))
        ch = FakeChannel()
        LinkChannel(ch, link, SECRET)
        ch.page(req(3, "GET", MESSAGES))
        await until(ended(ch, 3))
        a = answers(ch.sent)[3]
        assert a["end"] == "end" and a["head"]["s"] == 200
        assert a["head"]["h"]["content-type"] == "application/json"
        size = len(a["body"])
        assert size > 100_000 and int(a["head"]["h"]["content-length"]) == size
        assert a["frames"] == -(-size // lite_rtc.BODY_FRAME_BYTES) >= 7               # full frames, then the rest
        page = json.loads(a["body"])
        assert [m["text"][:3] for m in page["messages"]] == [f"{i:03d}" for i in range(link_chat.HISTORY_LIMIT)]
        # the query passes as it is: the cursor from that page gives nothing newer
        ch.page(req(4, "GET", MESSAGES, query=f"?after={page['cursor']}&wait=0"))
        await until(ended(ch, 4))
        assert json.loads(answers(ch.sent)[4]["body"])["messages"] == []
        link.close()

    asyncio.run(go())


@pytest.mark.parametrize("path", ["/api/link/jobs", "/api/link/upload", "/api/link/file/" + "a" * 32,
                                  "/api/link/controls", "/api/link/today", "/api/web/link/turn", "/api/link/turn/",
                                  "/api/link/turn/../jobs", "/", ""])
def test_a_path_outside_turn_and_messages_is_a_404_here(tmp_path: Path, path: str,
                                                          monkeypatch: pytest.MonkeyPatch) -> None:
    reached: list[str] = []
    serve = LinkChat.serve

    async def spy(self: LinkChat, method: str, p: str, *args: Any, **kw: Any) -> None:
        reached.append(p)
        await serve(self, method, p, *args, **kw)

    monkeypatch.setattr(LinkChat, "serve", spy)

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        link = door(tmp_path, inlet)
        ch = FakeChannel()
        LinkChannel(ch, link, SECRET)
        ch.page(req(5, "GET", path))
        ch.page(req(6, "POST", path, {"text": "hi"}))
        for rid in (5, 6):
            a = answers(ch.sent)[rid]                                               # answered at once, by the door
            assert a["head"]["s"] == 404 and a["end"] == "end"
            assert json.loads(a["body"]) == {"ok": False, "error": "not found"}
        assert reached == [] and inlet.heard == []
        ch.page(req(8, "GET", MESSAGES))                                           # positive control
        await until(ended(ch, 8))
        assert reached == [MESSAGES] and answers(ch.sent)[8]["head"]["s"] == 200
        link.close()

    asyncio.run(go())


def test_the_pages_origin_never_reaches_linkchat_and_the_peers_secret_does(tmp_path: Path,
                                                                          monkeypatch: pytest.MonkeyPatch) -> None:
    seen: list[tuple[dict[str, str], Any]] = []
    serve = LinkChat.serve

    async def spy(self: LinkChat, method: str, path: str, query: str, headers: dict[str, str], reader: Any,
                  writer: Any, token_check: Any, **kw: Any) -> None:
        seen.append((dict(headers), token_check))
        await serve(self, method, path, query, headers, reader, writer, token_check, **kw)

    monkeypatch.setattr(LinkChat, "serve", spy)

    async def go() -> None:
        link = door(tmp_path, FakeInlet(store_at(tmp_path)))
        ch = FakeChannel()
        LinkChannel(ch, link, SECRET)
        ch.page(req(1, "GET", MESSAGES, headers={"Origin": "https://evil.example", "Authorization": "Bearer stolen",
                                                 "Cookie": "eve_dev=CANARY-NOT-A-KEY", "Content-Length": "999",
                                                 "Accept": "application/json"}))
        await until(ended(ch, 1))
        assert answers(ch.sent)[1]["head"]["s"] == 200                              # LinkChat let it in
        headers, check = seen[0]
        assert headers == {"accept": "application/json", "authorization": f"Bearer {SECRET}"}
        assert check(SECRET) is True and check("stolen") is False and check("") is False
        # Positive control: LinkChat itself refuses any Origin, so the drop above is what let the page in.
        writer = ChannelWriter(LinkChannel(FakeChannel(), link, SECRET), 2)
        await serve(link, "GET", MESSAGES, "", {"origin": "https://evil.example", "authorization": f"Bearer {SECRET}"},
                    asyncio.StreamReader(), writer, lambda t: t == SECRET)
        assert writer.status == 403
        link.close()

    asyncio.run(go())


# ---- the limits ------------------------------------------------------------------------------------------------

def test_the_limits_on_one_request(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        link = door(tmp_path, inlet)
        ch = FakeChannel()
        LinkChannel(ch, link, SECRET)
        ch.page(req(1, "POST", TURN, "x" * (lite_rtc.MAX_BODY_BYTES + 1)))
        ch.page(req(2, "POST", TURN, "é" * (lite_rtc.MAX_BODY_BYTES // 2 + 1)))     # counted in UTF-8 bytes
        ch.page(req(3, "PUT", TURN, {"text": "hi"}))
        ch.page(req(4, "POST", TURN, {"text": "hi"}, headers={"x-bad": "a\r\nb"}))
        ch.page(req(5, "POST", TURN, {"text": "hi"}, headers={f"x-{i}": "1" for i in range(40)}))
        ch.page({**req(6, "GET", MESSAGES), "q": 5})
        ch.page({**req(7, "POST", TURN), "b": {"text": "hi"}})
        got = answers(ch.sent)
        assert {rid: got[rid]["head"]["s"] for rid in range(1, 8)} == {1: 413, 2: 413, 3: 405, 4: 400, 5: 400,
                                                                       6: 400, 7: 400}
        assert all(got[rid]["end"] == "end" for rid in range(1, 8))
        # a body of exactly the limit reaches LinkChat, which answers it as its own (the text is too long there)
        ch.page(req(8, "POST", TURN, json.dumps({"text": "x" * (lite_rtc.MAX_BODY_BYTES - 50)}).ljust(
            lite_rtc.MAX_BODY_BYTES)))
        await until(ended(ch, 8))
        a = answers(ch.sent)[8]
        assert a["head"]["s"] == 400 and "4000 characters" in json.loads(a["body"])["error"]
        # what cannot be answered (no id, not JSON, not text) is dropped, and the channel goes on
        before = len(ch.sent)
        for bad in (req(None, "GET", MESSAGES), req(-1, "GET", MESSAGES), req(1 << 32, "GET", MESSAGES),
                    req(True, "GET", MESSAGES), "not json", "[1, 2]", b"\x00\x01binary", {"t": "nope", "id": 9}):
            ch.page(bad)
        assert len(ch.sent) == before and inlet.heard == []
        ch.page(req(9, "GET", MESSAGES))
        await until(ended(ch, 9))
        assert answers(ch.sent)[9]["head"]["s"] == 200
        link.close()

    asyncio.run(go())


def test_eight_requests_run_at_once_and_a_ninth_waits_its_turn_elsewhere(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        link = door(tmp_path, inlet, poll_secs=0.05)
        ch = FakeChannel()
        lc = LinkChannel(ch, link, SECRET)
        ch.page(req(0, "GET", MESSAGES))
        await until(ended(ch, 0))
        cursor = json.loads(answers(ch.sent)[0]["body"])["cursor"]
        for rid in range(1, 9):                                                    # long polls: each holds a slot
            ch.page(req(rid, "GET", MESSAGES, query=f"after={cursor}&wait=20"))
        assert len(lc.requests) == lite_rtc.MAX_REQUESTS
        ch.page(req(9, "GET", MESSAGES, query=f"after={cursor}&wait=20"))
        a = answers(ch.sent)[9]
        assert a["head"]["s"] == 429 and a["end"] == "end" and "at most 8" in json.loads(a["body"])["error"]
        ch.page(req(1, "GET", MESSAGES, query=f"after={cursor}&wait=20"))         # an id still running: dropped
        assert 1 not in answers(ch.sent)
        ch.page({"t": "cancel", "id": 1})
        ch.page(req(10, "GET", MESSAGES, query=f"after={cursor}&wait=0"))         # its slot went to this one
        await until(ended(ch, 10))
        assert answers(ch.sent)[10]["head"]["s"] == 200 and 1 not in answers(ch.sent)
        tasks = [r.task for r in lc.requests.values()]
        ch.close()                                                                 # every long poll is cancelled
        assert lc.closed and lc.requests == {}
        await asyncio.wait([t for t in tasks if t is not None], timeout=2)
        assert all(t is not None and t.done() for t in tasks)
        assert all(answers(ch.sent).get(rid) is None for rid in range(1, 9))       # nothing went out after it
        link.close()

    asyncio.run(go())


def test_a_closed_channel_ends_its_turns_streams(tmp_path: Path) -> None:
    async def forever(inlet: FakeInlet, app: str) -> None:
        inlet.out(app, "Working on it.")

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), forever)
        link = door(tmp_path, inlet, ping_secs=30.0)
        ch = FakeChannel()
        lc = LinkChannel(ch, link, SECRET)
        ch.page(req(1, "POST", TURN, {"text": "hi", "client_id": "rtc-close-1"}))
        await until(lambda: b"Working" in answers(ch.sent).get(1, {}).get("body", b""))
        turn = link._turns["rtc-close-1"]
        assert turn.readers == 1
        ch.close()
        assert lc.closed
        await until(lambda: turn.readers == 0)
        ch.page(req(2, "GET", MESSAGES))                                           # a closed channel takes nothing
        await asyncio.sleep(0.05)
        assert 2 not in answers(ch.sent)
        link.close()

    asyncio.run(go())


def test_a_writer_drains_while_the_channel_is_behind() -> None:
    async def go() -> None:
        ch = FakeChannel()
        lc = LinkChannel(ch, None, SECRET)
        assert ch.bufferedAmountLowThreshold == lite_rtc.BUFFER_HIGH
        writer = ChannelWriter(lc, 1)
        ch.bufferedAmount = lite_rtc.BUFFER_HIGH + 1
        waiting = asyncio.ensure_future(writer.drain())
        await asyncio.sleep(0.05)
        assert not waiting.done()                                                  # the page is behind: wait
        ch.bufferedAmount = lite_rtc.BUFFER_HIGH
        ch.emit("bufferedamountlow")
        await asyncio.wait_for(waiting, 1)
        ch.bufferedAmount = lite_rtc.BUFFER_HIGH + 1
        waiting = asyncio.ensure_future(writer.drain())
        await asyncio.sleep(0.05)
        ch.close()                                                                 # a closed channel lets go
        await asyncio.wait_for(waiting, 1)
        with pytest.raises(ConnectionResetError):
            writer.write(b"HTTP/1.1 200 OK\r\n\r\n")                               # and the next write fails

    asyncio.run(go())


# ---- the writer's framing --------------------------------------------------------------------------------------

def writer_on(ch: FakeChannel, rid: int = 1) -> ChannelWriter:
    return ChannelWriter(LinkChannel(ch, None, SECRET), rid)


def test_the_writer_dechunks_a_stream_written_a_byte_at_a_time() -> None:
    async def go() -> None:
        ch = FakeChannel()
        w = writer_on(ch, 70000)
        pieces = [link_chat.sse("one"), link_chat.sse("two " * 5000), link_chat.DONE]
        raw = link_chat.STREAM_HEAD + b"".join(f"{len(p):x}\r\n".encode() + p + b"\r\n" for p in pieces) \
            + b"0\r\n\r\n"
        for i in range(len(raw)):
            w.write(raw[i:i + 1])
            await w.drain()
        a = answers(ch.sent)[70000]
        assert a["head"]["s"] == 200 and a["end"] == "end" and a["body"] == b"".join(pieces)
        assert "transfer-encoding" not in a["head"]["h"] and a["head"]["h"]["x-accel-buffering"] == "no"
        w.finish()                                                                 # already ended: nothing more
        w.write(b"after the end")
        assert answers(ch.sent)[70000]["end"] == "end"

    asyncio.run(go())


def test_the_writer_frames_a_content_length_body_and_a_body_to_the_close() -> None:
    ch = FakeChannel()
    w = writer_on(ch, 1)
    body = b"x" * (lite_rtc.BODY_FRAME_BYTES * 2 + 5)
    w.write(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: " + str(len(body)).encode()
            + b"\r\nConnection: close\r\n\r\n" + body[:100])
    assert answers(ch.sent)[1]["end"] is None                                      # not all of it yet
    w.write(body[100:])
    a = answers(ch.sent)[1]
    assert a["end"] == "end" and a["body"] == body and a["frames"] == 3
    assert a["head"]["h"] == {"content-type": "application/json", "content-length": str(len(body))}
    w2 = writer_on(ch, 2)
    w2.write(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nuntil the close")
    assert answers(ch.sent)[2]["end"] is None
    w2.finish()
    assert answers(ch.sent)[2]["end"] == "end" and answers(ch.sent)[2]["body"] == b"until the close"


@pytest.mark.parametrize("raw", [b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\nzz\r\n",
                                 b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabcXY",
                                 b"HTTP/1.1 OK\r\n\r\n", b"not http at all\r\n\r\n",
                                 b"HTTP/1.1 200 OK\r\nno colon here\r\n\r\n",
                                 b"HTTP/1.1 200 OK\r\nContent-Length: many\r\n\r\n"])
def test_an_answer_that_cannot_be_framed_ends_in_err(raw: bytes) -> None:
    ch = FakeChannel()
    w = writer_on(ch)
    with pytest.raises(ConnectionResetError):
        w.write(raw)
    assert answers(ch.sent)[1]["end"] == "err"
    with pytest.raises(ConnectionResetError):
        w.write(b"more")                                                           # LinkChat's stream stops here


def test_an_answer_cut_short_ends_in_err_and_a_cancelled_one_in_nothing() -> None:
    ch = FakeChannel()
    w = writer_on(ch, 1)
    w.write(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc")
    w.finish()
    assert answers(ch.sent)[1]["end"] == "err" and answers(ch.sent)[1]["body"] == b"abc"
    w2 = writer_on(ch, 2)
    w2.write(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n")
    w2.fail()
    assert answers(ch.sent)[2]["end"] == "err"
    w3 = writer_on(ch, 3)
    w3.dead = True                                                                 # cancelled by the page
    w3.finish()
    w3.fail()
    assert 3 not in answers(ch.sent)
    with pytest.raises(ConnectionResetError):
        w3.write(b"HTTP/1.1 200 OK\r\n\r\n")


def test_closing_the_writer_ends_the_answer_as_a_closed_connection_would() -> None:
    ch = FakeChannel()
    w = writer_on(ch, 1)
    w.write(b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nabc")
    w.close()                                                                      # mid-body: the page must know
    assert answers(ch.sent)[1]["end"] == "err" and w.is_closing()
    w2 = writer_on(ch, 2)
    w2.write(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nto the close")
    w2.close()                                                                     # the close is this body's end
    assert answers(ch.sent)[2]["end"] == "end" and answers(ch.sent)[2]["body"] == b"to the close"
    w3 = writer_on(ch, 3)
    w3.write(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
    w3.close()                                                                     # already whole: nothing more
    assert answers(ch.sent)[3]["end"] == "end"
    with pytest.raises(ConnectionResetError):
        w3.write(b"more")


class OnlyWriteAndDrain:
    """A writer with nothing but ``write`` and ``drain``: what LinkChat may use (lite_rtc.ChannelWriter's contract)."""

    def __init__(self, inner: ChannelWriter) -> None:
        self._inner = inner

    def write(self, data: bytes) -> None:
        self._inner.write(data)

    async def drain(self) -> None:
        await self._inner.drain()


def test_linkchat_uses_only_write_and_drain_on_the_paths_the_channel_forwards(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), two_lines)
        link = door(tmp_path, inlet)
        ch = FakeChannel()
        lc = LinkChannel(ch, link, SECRET)
        body = json.dumps({"text": "hi", "client_id": "rtc-strict-1"}).encode()
        cases = [("POST", TURN, body), ("GET", MESSAGES, b""), ("GET", TURN, b""), ("POST", TURN, b"{}"),
                 ("GET", MESSAGES + "", b"")]
        for rid, (method, path, data) in enumerate(cases, 1):
            reader = asyncio.StreamReader()
            reader.feed_data(data)
            headers = {"authorization": f"Bearer {SECRET}", **({"content-length": str(len(data))} if data else {})}
            writer = ChannelWriter(lc, rid)
            query = "after=nope" if rid == 5 else ""
            task = asyncio.ensure_future(link.serve(method, path, query, headers, reader,
                                                    OnlyWriteAndDrain(writer), lc.check))
            await until(lambda w=writer: w.done, 5)
            reader.feed_eof()
            await asyncio.wait_for(task, 5)
        got = answers(ch.sent)
        assert [got[rid]["head"]["s"] for rid in range(1, 6)] == [200, 200, 405, 400, 400]
        assert all(got[rid]["end"] == "end" for rid in range(1, 6))
        link.close()

    asyncio.run(go())
