"""link_chat.py: Eve Link's chat door. Its auth, a turn's SSE stream, the conversation read back from the
transcript (cursor, long poll, the 04:00 day), where an app turn's answer goes, and the OpenUI guide.

The real Mini App server on a loopback port and a raw HTTP client standing in for the app's URLSession. The
stream's mechanics run against a small fake inlet; reply routing, questions and the prompt run against the real
TelegramInlet with test_telegram.py's fakes and a real transcript store in a tmp folder."""
from __future__ import annotations

import asyncio
import json
import logging
import time
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Callable, Optional

import pytest
from test_chief import DESK_JOB, Clock, Fakes, dispatched, make, take
from test_chief import at as chief_clock
from test_telegram import CFG, OWNER, FakeApi, FakeCreate, FakeMemory, Rig, call, say

from eve_bridge import link_chat, stick_link, telegram
from eve_bridge.apps_maker import AppStore
from eve_bridge.link_chat import LinkChat, read_messages
from eve_bridge.miniapp import MiniAppConfig, MiniAppServer, SpendLedger, check_app_token
from eve_bridge.transcripts import TranscriptConfig, Transcripts

TG_TOKEN = "123456:AAsecretTOKENvalue"


# ---- the client ------------------------------------------------------------------------------------------

def dechunk(raw: bytes) -> bytes:
    """An HTTP/1.1 chunked body, checked as it is undone: every size line, every CRLF, and the 0 chunk at the end."""
    out, rest = b"", raw
    while True:
        size_line, sep, rest = rest.partition(b"\r\n")
        assert sep, "a chunk without its size line"
        size = int(size_line, 16)
        if size == 0:
            assert rest == b"\r\n", "the stream must end with the 0 chunk"
            return out
        out, rest = out + rest[:size], rest[size:]
        assert rest[:2] == b"\r\n", "a chunk without its CRLF"
        rest = rest[2:]


async def http(port: int, method: str, path: str, body: Any = None, *, token: Optional[str] = None,
               origin: Optional[str] = None) -> tuple[int, dict[str, str], bytes]:
    reader, writer = await asyncio.open_connection("127.0.0.1", port)
    data = json.dumps(body).encode() if isinstance(body, (dict, list)) else (body or b"")
    head = f"{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n"
    if token is not None:
        head += f"Authorization: Bearer {token}\r\n"
    if origin is not None:
        head += f"Origin: {origin}\r\n"
    if method == "POST":
        head += f"Content-Type: application/json\r\nContent-Length: {len(data)}\r\n"
    writer.write(head.encode() + b"\r\n" + data)
    await writer.drain()
    raw = await asyncio.wait_for(reader.read(), 20)        # the server closes when it is done
    writer.close()
    top, _, rest = raw.partition(b"\r\n\r\n")
    lines = top.decode().split("\r\n")
    headers = {k.strip().lower(): v.strip() for k, _, v in (ln.partition(":") for ln in lines[1:])}
    if headers.get("transfer-encoding") == "chunked":
        rest = dechunk(rest)
    return int(lines[0].split()[1]), headers, rest


def events(body: bytes) -> tuple[list[str], bool, int]:
    """(each chunk's content, whether [DONE] came last, how many pings). Every event must be well formed."""
    contents, done, pings = [], False, 0
    text = body.decode()
    assert text.endswith("\n\n")
    for event in text[:-2].split("\n\n"):
        assert not done, "nothing may follow [DONE]"
        if event == ": ping":
            pings += 1
        elif event == "data: [DONE]":
            done = True
        else:
            assert event.startswith("data: ") and "\n" not in event
            chunk = json.loads(event[len("data: "):])
            contents.append(chunk["choices"][0]["delta"]["content"])
            assert chunk["choices"][0]["index"] == 0
    return contents, done, pings


async def turn(port: int, token: str, text: str, client_id: str = "") -> tuple[int, dict[str, str], bytes]:
    return await http(port, "POST", link_chat.TURN_PATH, {"text": text, "client_id": client_id}, token=token)


async def messages(port: int, token: str, query: str = "") -> dict[str, Any]:
    status, _, body = await http(port, "GET", link_chat.MESSAGES_PATH + query, token=token)
    assert status == 200, body
    return json.loads(body)


# ---- the server ------------------------------------------------------------------------------------------

async def serve(tmp_path: Path, inlet: Any, **kw: Any) -> tuple[MiniAppServer, int, str]:
    cfg = MiniAppConfig(enabled=True, token=TG_TOKEN, owner_ids=frozenset({OWNER}), ledger_path=tmp_path / "l.json")
    server = MiniAppServer(cfg, SpendLedger(cfg.ledger_path), page=b"home", store=kw.pop("store", None))
    server.stick = stick_link.LinkToken(tmp_path / "stick-link.json")
    token = server.stick.ensure()
    kw.setdefault("hidden", link_chat.HiddenJobs(tmp_path / "hidden-jobs.json"))
    server.link = LinkChat(lambda: inlet, **kw)
    port = await server.start()
    return server, port, token


class FakeInlet:
    """The inlet as link_chat uses it: hear, owner_busy, app_out, transcripts and _chat_id. ``script`` runs as
    the turn would, in a task, and calls app_out the way TelegramInlet._to_app does."""

    def __init__(self, store: Any = None, script: Optional[Callable[["FakeInlet", str], Any]] = None) -> None:
        self._chat_id: Optional[int] = OWNER
        self.app_out: Any = None
        self.transcripts = store
        self.heard: list[tuple[str, bool, Optional[str]]] = []
        self.busy = False
        self.script = script
        self.delivered: list[bool] = []
        self.grades: list[str] = []
        self.chief: Any = None
        self.filed: dict[str, str] = {}               # app turn id -> the card its take_on filed (app_filed)

    def hear(self, text: str, *, on_call: bool = True, app: Optional[str] = None, grade: str = "auto") -> None:
        self.heard.append((text, on_call, app))
        self.grades.append(grade)
        if self.script is not None:
            self.busy = True
            asyncio.ensure_future(self.script(self, app or ""))

    def owner_busy(self) -> bool:
        return self.busy

    def app_filed(self, turn: str) -> Optional[str]:
        return self.filed.get(turn)

    def out(self, app: str, text: str, kind: str = "say") -> None:
        self.delivered.append(self.app_out(app, text, kind))


def store_at(tmp_path: Path) -> Transcripts:
    store = Transcripts(TranscriptConfig(enabled=True, root=tmp_path / "transcripts"))
    assert store.enabled
    return store


# ---- who may call ----------------------------------------------------------------------------------------

def test_only_the_app_with_the_link_token_gets_in(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        server, port, token = await serve(tmp_path, inlet)
        path = link_chat.MESSAGES_PATH
        assert (await http(port, "GET", path))[0] == 401                                # no token
        assert (await http(port, "GET", path, token="x" * 43))[0] == 401                # a wrong one
        assert (await http(port, "GET", path, token=token[:-1]))[0] == 401               # nearly right
        status, _, body = await http(port, "GET", path, token=token, origin="https://evil.example")
        assert status == 403 and b"Eve Link app only" in body                          # a web page, token or not
        assert (await http(port, "GET", path, token=token, origin="null"))[0] == 403
        assert (await http(port, "POST", link_chat.TURN_PATH, {"text": "hi"}, token=token,
                           origin="https://evil.example"))[0] == 403
        assert inlet.heard == []                                                         # nothing was heard
        assert (await http(port, "GET", path, token=token))[0] == 200                    # positive control
        await server.close()

    asyncio.run(go())


def test_a_wrong_scheme_an_unpaired_phone_and_no_link_door_are_refused(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        reader, writer = await asyncio.open_connection("127.0.0.1", port)
        writer.write(f"GET {link_chat.MESSAGES_PATH} HTTP/1.1\r\nAuthorization: Basic {token}\r\n\r\n".encode())
        assert (await reader.read()).startswith(b"HTTP/1.1 401")
        writer.close()
        server.stick = None                                                              # never paired
        assert (await http(port, "GET", link_chat.MESSAGES_PATH, token=token))[0] == 401
        server.link = None
        assert (await http(port, "GET", link_chat.MESSAGES_PATH, token=token))[0] == 404
        await server.close()

    asyncio.run(go())


def test_no_chat_running_is_a_503_with_a_reason(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet._chat_id = None
        server, port, token = await serve(tmp_path, inlet)
        for method, path, body in (("GET", link_chat.MESSAGES_PATH, None),
                                   ("POST", link_chat.TURN_PATH, {"text": "hi", "client_id": "a"})):
            status, headers, raw = await http(port, method, path, body, token=token)
            assert status == 503 and headers["content-type"] == "application/json"
            assert json.loads(raw) == {"ok": False, "error": link_chat.NO_CHAT}
        assert inlet.heard == []
        inlet._chat_id = OWNER
        assert (await http(port, "GET", link_chat.MESSAGES_PATH, token=token))[0] == 200  # positive control
        await server.close()
        no_inlet, port2, token2 = await serve(tmp_path / "b", None)
        assert (await http(port2, "GET", link_chat.MESSAGES_PATH, token=token2))[0] == 503
        await no_inlet.close()

    asyncio.run(go())


# ---- a turn ----------------------------------------------------------------------------------------------

async def two_lines(inlet: FakeInlet, app: str) -> None:
    await asyncio.sleep(0.02)
    inlet.out(app, "First line.")
    await asyncio.sleep(0.02)
    inlet.out(app, "Second line.")
    inlet.busy = False


def test_a_turn_is_heard_once_as_typed_and_streams_each_line_then_done(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), two_lines)
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01)
        status, headers, body = await turn(port, token, "  hello eve  ", "c-1")
        assert status == 200 and headers["content-type"] == "text/event-stream"
        assert headers["transfer-encoding"] == "chunked" and headers["cache-control"] == "no-store"
        contents, done, _ = events(body)
        assert contents == ["First line.", "\n\nSecond line."] and done        # a blank line between lines
        assert len(inlet.heard) == 1
        text, on_call, app = inlet.heard[0]
        assert text == "hello eve" and on_call is False and app             # trimmed, typed, the app's
        assert inlet.delivered == [True, True]                                 # a reader had both
        await server.close()

    asyncio.run(go())


def test_the_same_client_id_is_not_heard_again_and_gets_the_first_reply(tmp_path: Path) -> None:
    async def go() -> None:
        now = {"t": 1000.0}
        inlet = FakeInlet(store_at(tmp_path), two_lines)
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01, clock=lambda: now["t"])
        first = events((await turn(port, token, "hello", "same"))[2])
        again = events((await turn(port, token, "hello", "same"))[2])
        assert len(inlet.heard) == 1 and again == first and first[0]           # the first turn's reply, once heard
        both = await asyncio.gather(turn(port, token, "hi", "live"), turn(port, token, "hi", "live"))
        assert len(inlet.heard) == 2                                           # a retry while the first streams
        assert events(both[0][2]) == events(both[1][2]) == (["First line.", "\n\nSecond line."], True, 0)
        events_other = events((await turn(port, token, "hello", "other"))[2])
        assert len(inlet.heard) == 3 and events_other[1]                       # another id is another turn
        now["t"] += link_chat.DEDUPE_SECS + 1
        await turn(port, token, "hello", "same")
        assert len(inlet.heard) == 4                                           # ten minutes on, it is new
        await turn(port, token, "hello", "")
        await turn(port, token, "hello", "")
        assert len(inlet.heard) == 6                                           # no client_id: no dedupe
        await server.close()

    asyncio.run(go())


def test_a_turn_that_says_nothing_is_an_empty_completion(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))                                  # hears, starts nothing
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01)
        status, _, body = await turn(port, token, "hello", "c")
        assert status == 200 and events(body) == ([], True, 0)
        await server.close()

    asyncio.run(go())


def test_a_quiet_stream_is_kept_alive_and_ends_at_its_limit(tmp_path: Path) -> None:
    async def forever(inlet: FakeInlet, app: str) -> None:
        inlet.out(app, "Working on it.")

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), forever)                         # busy and never done
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01, ping_secs=0.05, turn_secs=0.4)
        t0 = time.monotonic()
        contents, done, pings = events((await turn(port, token, "hello", "c"))[2])
        assert contents == ["Working on it."] and done and pings >= 3
        assert 0.35 <= time.monotonic() - t0 < 5
        assert inlet.app_out("nope", "late", "say") is False                   # an unknown turn: to Telegram
        await server.close()

    asyncio.run(go())


@pytest.mark.parametrize("body,status", [
    ({"text": ""}, 400), ({"text": "   "}, 400), ({"text": "x" * 4001}, 400), ({"text": 5}, 400),
    ({"text": "hi", "client_id": "c" * 65}, 400), ({"text": "hi", "client_id": 7}, 400), (["hi"], 400),
    (b"not json", 400), (b"", 400), ({"text": "x" * 4000, "client_id": "c" * 64}, 200), ({"text": "hi"}, 200),
    ({"text": "hi", "grade": "fast"}, 400), ({"text": "hi", "grade": None}, 400), ({"text": "hi", "grade": 1}, 400),
    ({"text": "hi", "grade": "Think"}, 400), ({"text": "hi", "grade": "think"}, 200),
    ({"text": "hi", "grade": "instant"}, 200), ({"text": "hi", "grade": "auto"}, 200)])
def test_the_limits_on_a_turn(tmp_path: Path, body: Any, status: int) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01)
        assert (await http(port, "POST", link_chat.TURN_PATH, body, token=token))[0] == status
        assert len(inlet.heard) == (1 if status == 200 else 0)
        await server.close()

    asyncio.run(go())


def test_wrong_methods_and_paths(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        server, port, token = await serve(tmp_path, inlet)
        assert (await http(port, "GET", link_chat.TURN_PATH, token=token))[0] == 405
        assert (await http(port, "POST", link_chat.MESSAGES_PATH, {"x": 1}, token=token))[0] == 405
        assert (await http(port, "GET", "/api/link/other", token=token))[0] == 404
        assert (await http(port, "GET", link_chat.MESSAGES_PATH + "?after=bogus", token=token))[0] == 400
        assert (await http(port, "GET", link_chat.MESSAGES_PATH + "?wait=soon", token=token))[0] == 400
        await server.close()

    asyncio.run(go())


# ---- the conversation, read back ---------------------------------------------------------------------------

def at(day: str, hhmm: str) -> datetime:
    return datetime.strptime(f"{day} {hhmm}", "%Y-%m-%d %H:%M").astimezone()


def test_history_is_oldest_first_and_the_cursor_crosses_the_four_oclock_day(tmp_path: Path) -> None:
    store = store_at(tmp_path)
    store.append("telegram", "t-1", "owner", "say", "late at night", now=at("2026-10-08", "03:59"))
    store.append("telegram", "t-1", "system", "close", "", now=at("2026-10-08", "03:59"))
    store.append("voice", "v-1", "eve", "say", "good morning", now=at("2026-10-08", "04:01"))
    assert store.days() == ["2026-10-07", "2026-10-08"]                        # 03:59 belongs to the evening before
    rows, cursor = read_messages(store)
    assert [r["text"] for r in rows] == ["late at night", "good morning"]
    assert [(r["role"], r["via"], r["kind"]) for r in rows] == [("owner", "telegram", "say"), ("eve", "voice", "say")]
    assert rows[0]["id"] == "2026-10-07:0" and rows[1]["id"] == "2026-10-08:0" and cursor == "2026-10-08:0"
    assert abs(rows[0]["t"] - at("2026-10-08", "03:59").timestamp()) < 1
    after_first, cur = read_messages(store, "2026-10-07:0")
    assert [r["text"] for r in after_first] == ["good morning"] and cur == "2026-10-08:0"
    assert read_messages(store, cur) == ([], cur)                              # nothing new: the same cursor
    store.append("telegram", "t-2", "owner", "say", "and again", now=at("2026-10-08", "09:00"))
    newer, cur2 = read_messages(store, cur)
    assert [r["text"] for r in newer] == ["and again"] and cur2 == "2026-10-08:1"


def test_history_is_the_newest_hundred_lines_however_far_back(tmp_path: Path) -> None:
    store = store_at(tmp_path)
    for n, day in enumerate(("2026-10-01", "2026-10-02", "2026-10-03", "2026-10-04")):
        store.append("telegram", "t", "owner", "say", f"day {n}", now=at(day, "12:00"))
    # every day, not only the last three (owner, 2026-10-13: "keep all chat history, i want infinite scroll")
    assert [r["text"] for r in read_messages(store)[0]] == ["day 0", "day 1", "day 2", "day 3"]
    for i in range(250):
        store.append("telegram", "t", "eve", "say", f"line {i}", now=at("2026-10-04", "13:00"))
    rows, cursor = read_messages(store)
    assert len(rows) == link_chat.HISTORY_LIMIT and rows[-1]["text"] == "line 249" and cursor == "2026-10-04:250"
    page, cur = read_messages(store, "2026-10-01:-1")
    assert len(page) == link_chat.PAGE_LIMIT and page[0]["text"] == "day 0"   # a long gap comes in pages
    assert cur == page[-1]["id"]
    rest, end = read_messages(store, cur)
    assert len(page) + len(rest) == 254 and rest[-1]["text"] == "line 249" and end == cursor


def test_only_what_was_said_is_shown_and_forgotten_lines_are_left_out(tmp_path: Path) -> None:
    store = store_at(tmp_path)
    when = at("2026-10-08", "10:00")
    store.append("telegram", "t", "owner", "say", "my secret plan", now=when)
    store.append("telegram", "t", "owner", "say", "keep this", now=when)
    store.append("telegram", "t", "owner", "command", "stealth on", now=when)
    store.append("telegram", "t", "claude", "relay", "Claude did a thing", now=when)
    store.append("bus", "b", "system", "say", "a program's line", now=when)
    store.append("telegram", "t", "eve", "tool", "searched", "web_search", now=when)
    store.append("telegram", "t", "owner", "image", "a photo", now=when)
    store.append("telegram", "t", "eve", "say", "from the app", now=when, via="app")
    assert store.redact("secret plan") == 1
    rows, cursor = read_messages(store)
    assert [(r["text"], r["kind"], r["via"]) for r in rows] == [
        ("keep this", "say", "telegram"), ("Claude did a thing", "say", "other"), ("searched", "tool", "telegram"),
        ("a photo", "image", "telegram"), ("from the app", "say", "app")]
    assert [r["id"] for r in rows] == ["2026-10-08:1", "2026-10-08:3", "2026-10-08:5", "2026-10-08:6",
                                       "2026-10-08:7"]
    assert cursor == "2026-10-08:7"


def test_claude_on_shows_both_sides_of_the_relay_in_the_app(tmp_path: Path) -> None:
    store = store_at(tmp_path)
    when = at("2026-10-08", "20:24")
    store.append("telegram", "t", "owner", "command", "Claude on", now=when, via="app")
    store.append("telegram", "t", "owner", "relay", "What are the capabilities of openui here", now=when, via="app")
    store.append("telegram", "t", "claude", "relay", "OpenUI draws the chat's replies.", now=when)
    rows, _ = read_messages(store)
    assert [(r["role"], r["text"], r["kind"]) for r in rows] == [
        ("owner", "What are the capabilities of openui here", "say"),
        ("eve", "OpenUI draws the chat's replies.", "say")]
    assert rows[0]["via"] == "app" and rows[1]["via"] == "other"


def test_a_line_without_via_still_loads_and_via_is_written_only_when_given(tmp_path: Path) -> None:
    store = store_at(tmp_path)
    store.append("telegram", "t", "owner", "say", "old style", now=at("2026-10-08", "10:00"))
    store.append("telegram", "t", "owner", "say", "new style", now=at("2026-10-08", "10:00"), via="app")
    raw = [json.loads(x) for x in store.path_of("2026-10-08").read_text().splitlines()]
    assert "via" not in raw[0] and raw[1]["via"] == "app"
    assert [ln["text"] for ln in store.lines("2026-10-08")] == ["old style", "new style"]


def test_a_long_poll_wakes_on_a_new_line_and_times_out_empty_without_leaking(tmp_path: Path) -> None:
    async def go() -> None:
        store = store_at(tmp_path)
        store.append("telegram", "t", "owner", "say", "first")
        inlet = FakeInlet(store)
        server, port, token = await serve(tmp_path, inlet, poll_secs=30.0)    # a wake, never the re-read
        start = await messages(port, token)
        cursor = start["cursor"]
        assert [m["text"] for m in start["messages"]] == ["first"] and start["busy"] is False

        async def later() -> None:
            await asyncio.sleep(0.3)
            store.append("telegram", "t", "eve", "say", "second")

        t0 = time.monotonic()
        got, _ = await asyncio.gather(messages(port, token, f"?after={cursor}&wait=20"), later())
        assert [m["text"] for m in got["messages"]] == ["second"] and 0.25 <= time.monotonic() - t0 < 5
        t0 = time.monotonic()
        empty = await messages(port, token, f"?after={got['cursor']}&wait=0.4")
        assert empty == {"messages": [], "cursor": got["cursor"], "busy": False}
        assert 0.35 <= time.monotonic() - t0 < 5
        assert (await messages(port, token, f"?after={got['cursor']}"))["messages"] == []   # wait=0: at once
        assert len(store.listeners) == 1                                       # one wake-up, however many polls
        await asyncio.sleep(0.05)
        others = [t for t in asyncio.all_tasks() if t is not asyncio.current_task()]
        assert others == [], others                                            # no waiter left behind
        await server.close()

    asyncio.run(go())


def test_a_long_poll_also_sees_a_write_from_another_process(tmp_path: Path) -> None:
    async def go() -> None:
        store = store_at(tmp_path)
        store.append("telegram", "t", "owner", "say", "first")
        server, port, token = await serve(tmp_path, FakeInlet(store), poll_secs=0.1)
        cursor = (await messages(port, token))["cursor"]
        other = Transcripts(TranscriptConfig(enabled=True, root=tmp_path / "transcripts"))   # no listener

        async def later() -> None:
            await asyncio.sleep(0.3)
            other.append("telegram", "t", "eve", "say", "from elsewhere")

        got, _ = await asyncio.gather(messages(port, token, f"?after={cursor}&wait=10"), later())
        assert [m["text"] for m in got["messages"]] == ["from elsewhere"]
        await server.close()

    asyncio.run(go())


def test_busy_is_a_turn_in_flight_and_memory_off_is_a_503(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        server, port, token = await serve(tmp_path, inlet)
        assert (await messages(port, token))["busy"] is False
        inlet.busy = True
        assert (await messages(port, token))["busy"] is True
        inlet.transcripts = None
        status, _, body = await http(port, "GET", link_chat.MESSAGES_PATH, token=token)
        assert status == 503 and json.loads(body)["error"] == link_chat.NO_MEMORY
        await server.close()

    asyncio.run(go())


def test_a_long_poll_returns_when_busy_clears_not_at_its_timeout(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet.busy = True
        server, port, token = await serve(tmp_path, inlet, poll_secs=0.05)
        cursor = (await messages(port, token))["cursor"]

        async def clear() -> None:
            await asyncio.sleep(0.2)
            inlet.busy = False

        clearing = asyncio.ensure_future(clear())
        t0 = time.monotonic()
        got = await messages(port, token, f"?after={cursor}&wait=20")
        assert got["messages"] == [] and got["busy"] is False and time.monotonic() - t0 < 5
        await clearing
        await server.close()

    asyncio.run(go())


# ---- with the real chat ------------------------------------------------------------------------------------

def real_rig(tmp_path: Path, *responses: Any, **kw: Any) -> tuple[Rig, FakeMemory]:
    memory = FakeMemory(tmp_path / "m")
    rig = Rig(FakeApi(), FakeCreate(*responses), memory=memory, **kw)
    rig.inlet._chat_id = OWNER
    return rig, memory


def test_an_app_turn_is_answered_in_the_app_not_in_telegram_and_marked_via_app(tmp_path: Path) -> None:
    rig, memory = real_rig(tmp_path, say("Hello from eve."), say("Hello in Telegram."))

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        status, _, body = await turn(port, token, "hi from the phone", "c-1")
        assert status == 200 and events(body) == (["Hello from eve."], True, 0)
        assert rig.api.sent == [] and rig.api.typings == []                   # nothing in Telegram, not even typing
        assert len(rig.create.requests) == 1                                   # heard once
        app_lines = [(ln["who"], ln["text"], ln.get("via")) for ln in memory.said()]
        assert app_lines == [("owner", "hi from the phone", "app"), ("eve", "Hello from eve.", "app")]
        # positive control: the same words typed on the Mac (not the app) are answered in Telegram, as before
        rig.inlet.hear("hi from the Mac", on_call=False)
        for _ in range(200):
            await asyncio.sleep(0.01)
            if rig.api.sent:
                break
        assert rig.api.sent == [(OWNER, "Hello in Telegram.")]
        assert [ln.get("via") for ln in memory.said()[2:]] == [None, None]
        got = await messages(port, token)
        assert [(m["role"], m["via"]) for m in got["messages"]] == [
            ("owner", "app"), ("eve", "app"), ("owner", "telegram"), ("eve", "telegram")]
        await server.close()

    asyncio.run(go())


def test_an_app_turns_line_nobody_reads_goes_to_telegram(tmp_path: Path) -> None:
    rig, _ = real_rig(tmp_path, say("Nobody is reading."))

    async def go() -> None:
        link = LinkChat(lambda: rig.inlet)
        rig.inlet.app_out = link.deliver
        rig.inlet.hear("hello", on_call=False, app="a-turn-the-link-never-opened")
        for _ in range(200):
            await asyncio.sleep(0.01)
            if rig.api.sent:
                break
        assert rig.api.sent == [(OWNER, "Nobody is reading.")]                 # never lost

    asyncio.run(go())


def test_a_question_an_app_turn_asks_is_answered_by_the_next_text_from_the_app(tmp_path: Path) -> None:
    rig, memory = real_rig(tmp_path)
    answers: list[str] = []

    async def asks() -> dict[str, Any]:
        answer = await rig.inlet._ask_user("Should I go ahead: book the table?", OWNER, choices=telegram.YES_NO,
                                           firm=True, title="Confirm")
        answers.append(answer)
        return say(f"Booked. You said {answer}.")

    rig.create.responses.append(asks)

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        first = events((await turn(port, token, "book a table", "c-1"))[2])
        assert first[1] and len(first[0]) == 1                                # it ends on the question
        assert "Should I go ahead: book the table?" in first[0][0] and 'Reply "yes" or "no".' in first[0][0]
        assert rig.inlet._awaiting_answer and answers == []
        second = events((await turn(port, token, "yes", "c-2"))[2])
        assert answers == ["yes"]
        assert second == (["Booked. You said yes."], True, 0)                  # the rest of the reply, here
        assert rig.api.sent == []                                               # no Telegram copy of any of it
        assert len(rig.create.requests) == 1                                    # "yes" was an answer, not a turn
        assert not rig.inlet.owner_busy()
        assert ("owner", "yes") in [(ln["who"], ln["text"]) for ln in memory.said()]
        await server.close()

    asyncio.run(go())


def test_a_question_asked_in_telegram_is_answered_from_the_app(tmp_path: Path) -> None:
    rig, _ = real_rig(tmp_path)

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        ask = asyncio.ensure_future(rig.inlet.chief_ask("c1, Order the desk: order it.\nGo?"))
        for _ in range(100):
            await asyncio.sleep(0.01)
            if rig.inlet._awaiting_answer:
                break
        assert rig.api.buttons and rig.api.buttons[-1] == ["Yes", "No"]       # the Go is in Telegram, as before
        status, _, body = await turn(port, token, "yes", "c-1")
        assert status == 200 and events(body)[1]
        assert await asyncio.wait_for(ask, 5) == "yes"
        await server.close()

    asyncio.run(go())


def test_a_new_request_typed_in_the_app_while_a_go_waits_is_a_turn_not_dropped(tmp_path: Path) -> None:
    rig, _ = real_rig(tmp_path, say("Try the lentil soup."))

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        ask = asyncio.ensure_future(rig.inlet.chief_ask("c1, Order the desk: order it.\nGo?"))
        for _ in range(100):
            await asyncio.sleep(0.01)
            if rig.inlet._awaiting_answer:
                break
        assert rig.inlet._awaiting_answer
        assert (await messages(port, token))["busy"] is False                   # waiting on the owner: no dots
        status, _, body = await turn(port, token, "what should I cook tonight?", "c-1")
        assert status == 200 and events(body) == (["Try the lentil soup."], True, 0)  # heard, answered in the app
        assert len(rig.create.requests) == 1
        assert not ask.done()                                                   # the Go still waits for its answer
        ask.cancel()
        await server.close()

    asyncio.run(go())


def test_closing_the_server_ends_an_open_stream_with_done(tmp_path: Path) -> None:
    async def forever(inlet: FakeInlet, app: str) -> None:
        inlet.out(app, "Working on it.")

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), forever)                         # busy and never done
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01)
        pending = asyncio.ensure_future(turn(port, token, "hello", "c"))
        for _ in range(200):
            await asyncio.sleep(0.01)
            if inlet.delivered:
                break
        await server.close()
        status, _, body = await pending                                        # dechunk checks the 0 chunk
        assert status == 200 and events(body) == (["Working on it."], True, 0)

    asyncio.run(go())


def test_openui_programs_are_one_plain_line_outside_the_app() -> None:
    p = link_chat.OPENUI_PLACEHOLDER
    fenced = "Here it is.\n\n```openui-lang\nroot = Card([t])\nt = Table([])\n```\n\nAnything else?"
    assert link_chat.strip_openui(fenced) == f"Here it is.\n\n{p}\n\nAnything else?"
    assert link_chat.strip_openui("root = Card([])\n") == p                     # a bare program
    assert link_chat.strip_openui("Look:\n```openui-lang\nroot = Card([])") == f"Look:\n{p}"   # unclosed
    plain = "A list:\n\n- one\n- two\n\n```python\nprint(1)\n```"
    assert link_chat.strip_openui(plain) == plain                               # markdown and code untouched


def test_an_app_reply_nobody_reads_reaches_telegram_and_history_without_its_program(tmp_path: Path) -> None:
    reply = "Your week:\n\n```openui-lang\nroot = Card([t])\nt = Table([])\n```"
    rig, memory = real_rig(tmp_path, say(reply))

    async def go() -> None:
        link = LinkChat(lambda: rig.inlet)
        rig.inlet.app_out = link.deliver
        rig.inlet.hear("my week", on_call=False, app="a-turn-the-link-never-opened")
        for _ in range(200):
            await asyncio.sleep(0.01)
            if rig.api.sent:
                break

    asyncio.run(go())
    plain = f"Your week:\n\n{link_chat.OPENUI_PLACEHOLDER}"
    assert rig.api.sent == [(OWNER, plain)]                                     # no raw program in Telegram
    assert ("eve", reply) in rig.inlet.turns                                  # kept whole; each turn reads it
    assert ("eve", reply) in [(ln["who"], ln["text"]) for ln in memory.said()]  # the transcript keeps it all


# ---- eve's blocks in the history: as written for an app turn, one plain line for the rest ---------------------

DINNERS = """Three quick ones.
```openui-lang
root = Card([header, list, choose])
header = CardHeader("Three quick dinners", "Under 30 minutes")
list = ListBlock([d1, d2, d3], "number")
d1 = ListItem("Egg fried rice", "Leftover rice, eggs, frozen peas")
d2 = ListItem("Pasta aglio e olio", "Spaghetti, garlic, chili, olive oil")
d3 = ListItem("Black bean tacos", "Tortillas, beans, salsa, cheese")
choose = Buttons([b1, b2, b3], "row")
b1 = Button("Fried rice", Action([@ToAssistant("Give me the fried rice recipe")]), "secondary")
b2 = Button("Pasta", Action([@ToAssistant("Give me the pasta recipe")]), "secondary")
b3 = Button("Tacos", Action([@ToAssistant("Give me the taco recipe")]), "secondary")
```"""


def chat_texts(request: dict[str, Any]) -> list[tuple[str, str]]:
    """(role, text) of every message item in a request's input."""
    return [(item["role"], " ".join(c.get("text", "") for c in item["content"]))
            for item in request["input"] if item.get("type") == "message"]


def logged_rig(tmp_path: Path, with_log: bool, *responses: Any) -> Rig:
    from eve_bridge.chat_log import ChatLog

    kw: dict[str, Any] = {"chat_log": ChatLog(tmp_path / "chat" / "log.json", wall=lambda: 1_790_000_000.0)} \
        if with_log else {}
    rig = Rig(FakeApi(), FakeCreate(*responses), **kw)
    rig.inlet._chat_id = OWNER
    rig.inlet.app_out = lambda turn_id, text, kind: True                      # the app is reading
    return rig


async def app_turn(rig: Rig, text: str, n: int) -> None:
    mark = telegram._FROM_APP.set(f"app-turn-{n}")
    try:
        await rig.inlet._turn(telegram.Inbound(OWNER, OWNER, text, message_id=n))
    finally:
        telegram._FROM_APP.reset(mark)


@pytest.mark.parametrize("with_log", [False, True], ids=["ram", "chat-log"])
def test_an_app_follow_up_sees_the_options_its_last_block_offered(tmp_path: Path, with_log: bool) -> None:
    rig = logged_rig(tmp_path, with_log, say(DINNERS), say("Here is the pasta recipe."))

    async def go() -> None:
        await app_turn(rig, "quick dinner ideas", 1)
        await app_turn(rig, "the second one", 2)

    asyncio.run(go())
    shown = chat_texts(rig.create.requests[1])
    mine = [text for role, text in shown if role == "assistant"]
    assert mine == [DINNERS]                                                    # the block, as written
    for option in ("Egg fried rice", "Pasta aglio e olio", "Black bean tacos", "Give me the pasta recipe"):
        assert option in mine[0]
    everything = json.dumps(rig.create.requests[1]["input"])
    assert link_chat.OPENUI_PLACEHOLDER not in everything and link_chat.OPENUI_GONE not in everything


@pytest.mark.parametrize("with_log", [False, True], ids=["ram", "chat-log"])
def test_a_telegram_turn_after_an_app_block_still_reads_the_placeholder(tmp_path: Path, with_log: bool) -> None:
    rig = logged_rig(tmp_path, with_log, say(DINNERS), say("Sure."))

    async def go() -> None:
        await app_turn(rig, "quick dinner ideas", 1)
        await rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "thanks", message_id=2))   # typed in Telegram

    asyncio.run(go())
    mine = [text for role, text in chat_texts(rig.create.requests[1]) if role == "assistant"]
    assert mine == [f"Three quick ones.\n{link_chat.OPENUI_PLACEHOLDER}"]
    assert "openui-lang" not in json.dumps(rig.create.requests[1]["input"])
    assert "Egg fried rice" not in json.dumps(rig.create.requests[1]["input"])


def test_recent_messages_reads_blocks_the_way_the_turn_does(tmp_path: Path) -> None:
    rig = logged_rig(tmp_path, True, say(DINNERS))
    asyncio.run(app_turn(rig, "quick dinner ideas", 1))

    def recent(app: bool) -> str:
        async def go() -> dict[str, Any]:
            mark = telegram._FROM_APP.set("app-turn-9") if app else None
            try:
                return await rig.inlet._tool_recent_messages("recent_messages", {}, OWNER)
            finally:
                if mark is not None:
                    telegram._FROM_APP.reset(mark)
        out = asyncio.run(go())
        return next(m["text"] for m in out["messages"] if m["who"] == "you (eve)")

    assert "Pasta aglio e olio" in recent(app=True)
    assert recent(app=False) == f"Three quick ones.\n{link_chat.OPENUI_PLACEHOLDER}"


def test_history_text_per_channel() -> None:
    p, gone = link_chat.OPENUI_PLACEHOLDER, link_chat.OPENUI_GONE
    assert link_chat.history_text(DINNERS, app=True) == DINNERS
    assert link_chat.history_text(DINNERS, app=False) == f"Three quick ones.\n{p}"
    # a line logged before app history kept blocks: an app turn is not told to "open Eve Link"
    assert link_chat.history_text(f"Your week:\n\n{p}", app=True) == f"Your week:\n\n{gone}"
    assert link_chat.history_text(f"Your week:\n\n{p}", app=False) == f"Your week:\n\n{p}"
    assert link_chat.history_text("Hi.", app=True) == link_chat.history_text("Hi.", app=False) == "Hi."


def test_the_openui_guide_is_in_app_turns_only(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    guide = tmp_path / "openui_prompt.txt"
    guide.write_text("OPENUI LANG GUIDE: Table, Chart, Steps.\n")
    monkeypatch.setattr(link_chat, "PROMPT_PATH", guide)
    monkeypatch.setattr(link_chat, "_prompt", {"key": None, "text": "", "warned": False})
    rig, _ = real_rig(tmp_path, say("From the app."), say("From Telegram."))

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        await turn(port, token, "show me a table", "c-1")
        rig.inlet.hear("show me a table", on_call=False)
        for _ in range(200):
            await asyncio.sleep(0.01)
            if len(rig.create.requests) == 2 and rig.api.sent:
                break
        await server.close()

    asyncio.run(go())
    app_turn, telegram_turn = (r["instructions"] for r in rig.create.requests)
    assert "OPENUI LANG GUIDE: Table, Chart, Steps." in app_turn and link_chat.APP_LINE in app_turn
    assert "OPENUI LANG GUIDE" not in telegram_turn and link_chat.APP_LINE not in telegram_turn
    # the only differences: the block, and the texting style swapped for the app's (test_openui_eval.py)
    assert (app_turn.replace("\n\n" + link_chat.app_block(), "").replace(telegram.APP_STYLE, telegram._TEXT_STYLE)
            == telegram_turn)


def test_no_guide_file_means_no_addition_logged_once(tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
                                                     caplog: pytest.LogCaptureFixture) -> None:
    monkeypatch.setattr(link_chat, "PROMPT_PATH", tmp_path / "absent.txt")
    monkeypatch.setattr(link_chat, "_prompt", {"key": None, "text": "", "warned": False})
    with caplog.at_level(logging.WARNING, logger="eve_bridge.link_chat"):
        assert link_chat.app_block() == "" and link_chat.app_block() == ""
    assert sum("openui_prompt" in r.getMessage() or "absent.txt" in r.getMessage() for r in caplog.records) == 1
    (tmp_path / "absent.txt").write_text("   \n")
    assert link_chat.app_block() == ""                                          # empty counts as absent
    (tmp_path / "absent.txt").write_text("GUIDE")
    assert link_chat.app_block() == link_chat.APP_RULE + "\n\nGUIDE\n\n" + link_chat.APP_LINE   # a new file is picked up


def test_the_shipped_guide_is_package_data() -> None:
    import tomllib

    pyproject = Path(__file__).resolve().parents[1] / "pyproject.toml"
    data = tomllib.loads(pyproject.read_text())["tool"]["setuptools"]["package-data"]["eve_bridge"]
    assert "openui_prompt.txt" in data and link_chat.PROMPT_PATH.exists()


def test_days_cross_at_four_even_with_a_clock_near_midnight(tmp_path: Path) -> None:
    store = store_at(tmp_path)
    base = at("2026-10-08", "00:30")
    for i, minutes in enumerate((0, 200, 220)):                                 # 00:30, 03:50, 04:10
        store.append("telegram", "t", "owner", "say", f"m{i}", now=base + timedelta(minutes=minutes))
    rows, _ = read_messages(store)
    assert [(r["id"], r["text"]) for r in rows] == [("2026-10-07:0", "m0"), ("2026-10-07:1", "m1"),
                                                    ("2026-10-08:0", "m2")]


# ---- the speed dial: a turn's grade -------------------------------------------------------------------------

def test_the_grade_reaches_the_inlet_and_auto_is_the_default(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01)
        await turn(port, token, "hi", "a")
        for grade in ("instant", "think", "auto"):
            body = {"text": "hi", "client_id": grade, "grade": grade}
            assert (await http(port, "POST", link_chat.TURN_PATH, body, token=token))[0] == 200
        assert inlet.grades == ["auto", "instant", "think", "auto"]
        await server.close()

    asyncio.run(go())


def test_each_grade_sets_the_turns_model_and_effort_and_instant_offers_no_think_hard(tmp_path: Path) -> None:
    rig, _ = real_rig(tmp_path, say("auto"), say("instant"), say("think"), say("telegram"))

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        for grade in ("auto", "instant", "think"):
            got = events((await http(port, "POST", link_chat.TURN_PATH,
                                     {"text": f"a question, {grade}", "client_id": grade, "grade": grade},
                                     token=token))[2])
            assert got == ([grade], True, 0)
        rig.inlet.hear("from the Mac", on_call=False)                 # not an app turn: never graded
        for _ in range(200):
            await asyncio.sleep(0.01)
            if len(rig.create.requests) == 4:
                break
        await server.close()

    asyncio.run(go())
    auto, instant, think, plain = rig.create.requests
    names = [[t.get("name") for t in r["tools"]] for r in (auto, instant, think, plain)]
    assert (auto["model"], auto["reasoning"]) == (CFG.model, {"effort": CFG.effort})      # today's, exactly
    assert "think_hard" in names[0] and names[0] == names[3]
    assert {k: v for k, v in auto.items() if k != "input"} == {k: v for k, v in plain.items() if k != "input"} | {
        "instructions": auto["instructions"]}                                              # only the app's guide differs
    assert (instant["model"], instant["reasoning"]) == (CFG.model, {"effort": "low"})
    assert "think_hard" not in names[1] and set(names[1]) == set(names[0]) - {"think_hard"}
    assert (think["model"], think["reasoning"]) == ("gpt-6.1-sol", {"effort": "medium"})
    assert names[2] == names[0] and think["instructions"] == auto["instructions"]


def test_a_think_hard_call_on_instant_is_refused_and_on_auto_it_runs(tmp_path: Path) -> None:
    rig, _ = real_rig(tmp_path, call("think_hard", {"question": "prove it"}), say("Here is a quick answer."))
    thought: list[str] = []

    async def fake_think(question: str, chat_id: Optional[int] = None) -> dict[str, Any]:
        thought.append(question)
        return {"ok": True, "answer": "deep"}

    rig.inlet._think_hard = fake_think                                 # type: ignore[method-assign]

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        got = events((await http(port, "POST", link_chat.TURN_PATH,
                                 {"text": "prove the theorem", "client_id": "i", "grade": "instant"},
                                 token=token))[2])
        assert got == (["Here is a quick answer."], True, 0)
        rig.create.responses += [call("think_hard", {"question": "prove it"}), say("Thought about it.")]
        got = events((await http(port, "POST", link_chat.TURN_PATH,
                                 {"text": "prove the theorem", "client_id": "a", "grade": "auto"},
                                 token=token))[2])
        assert got == (["Thought about it."], True, 0)
        await server.close()

    asyncio.run(go())
    assert thought == ["prove it"]                                     # only the auto turn ran it (positive control)
    refused = json.loads(next(i["output"] for i in rig.create.requests[1]["input"]
                              if i.get("type") == "function_call_output"))
    assert refused["ok"] is False and "think_hard" not in refused["reason"]


def test_quick_answers_still_come_first_in_every_grade(tmp_path: Path) -> None:
    rig, _ = real_rig(tmp_path)

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        for grade in ("auto", "instant", "think"):
            got = events((await http(port, "POST", link_chat.TURN_PATH,
                                     {"text": "what is 12 times 7", "client_id": grade, "grade": grade},
                                     token=token))[2])
            assert got[1] and len(got[0]) == 1 and "84" in got[0][0]
        await server.close()

    asyncio.run(go())
    assert rig.create.requests == []                                   # no model call in any grade


def test_the_app_think_settings_come_from_the_environment() -> None:
    base = {"EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": "t", "EVE_TELEGRAM_OWNER": "1"}
    cfg = telegram.configured(base)
    assert (cfg.app_think_model, cfg.app_think_effort) == ("gpt-6.1-sol", "medium")
    cfg = telegram.configured({**base, "EVE_APP_THINK_MODEL": "gpt-6-astra", "EVE_APP_THINK_EFFORT": "XHigh"})
    assert (cfg.app_think_model, cfg.app_think_effort) == ("gpt-6-astra", "xhigh")
    assert telegram.configured({**base, "EVE_APP_THINK_EFFORT": "huge"}).app_think_effort == "medium"
    assert telegram.GRADES == link_chat.GRADES
    payload = {"model": "m", "reasoning": {"effort": "high"}, "tools": [{"name": "think_hard"}, {"name": "x"}]}
    assert telegram.graded(payload, cfg, "auto") is payload
    assert telegram.graded(payload, cfg, "bogus") is payload


# ---- the chief's cards: the Work view ------------------------------------------------------------------------

def chief_at(tmp_path: Path, hour: int = 10) -> tuple[Any, Fakes, Clock]:
    """A real Chief (test_chief's fakes) with three Standing desk cards, each waiting on its Go."""
    f, clock = Fakes(), Clock(chief_clock(hour))
    c = make(tmp_path / "chief", f, clock)

    async def three() -> None:
        for _ in range(3):
            await take(c)
        await c.drain()

    asyncio.run(three())
    return c, f, clock


async def jobs(port: int, token: str) -> dict[str, Any]:
    status, _, body = await http(port, "GET", link_chat.JOBS_PATH, token=token)
    assert status == 200, body
    return json.loads(body)


async def act(port: int, token: str, cid: str, verb: str, body: Any = None) -> tuple[int, dict[str, Any]]:
    status, _, raw = await http(port, "POST", f"{link_chat.JOBS_PATH}/{cid}/{verb}", body, token=token)
    return status, json.loads(raw)


def test_every_new_path_needs_the_link_token_and_no_origin(tmp_path: Path) -> None:
    c, _, _ = chief_at(tmp_path)
    store = AppStore(tmp_path / "apps")
    store.save("<title>Tip jar</title><p>tips</p>", "a tip jar", slug="tip-jar")

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet, store=store)
        doors = (("GET", link_chat.JOBS_PATH), ("POST", f"{link_chat.JOBS_PATH}/c1/drop"),
                 ("GET", link_chat.APPS_PATH), ("POST", f"{link_chat.APPS_PATH}/tip-jar/open"))
        for method, path in doors:
            assert (await http(port, method, path))[0] == 401, path                           # no token
            assert (await http(port, method, path, token=token[:-1]))[0] == 401, path          # nearly right
            assert (await http(port, method, path, token=token, origin="https://evil.example"))[0] == 403, path
            assert (await http(port, method, path, token=token, origin="null"))[0] == 403, path
        assert c.ledger.get("c1").status == "waiting"                                         # nothing was done
        for method, path in doors:
            assert (await http(port, method, path, token=token))[0] == 200, path               # positive controls
        assert c.ledger.get("c1").status == "dropped"
        await server.close()

    asyncio.run(go())


def test_jobs_list_open_cards_first_needs_you_first_then_the_finished(tmp_path: Path) -> None:
    c, _, clock = chief_at(tmp_path)

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet)
        got = await jobs(port, token)
        assert [j["id"] for j in got["jobs"]] == ["c1", "c2", "c3"] and got["needs_you"] == 3
        first = got["jobs"][0]
        assert first == {**c._as_data(c.ledger.get("c1")), **{k: first[k] for k in (
            "rev", "updated", "line", "steps", "steps_done", "needs_you", "actions")}}      # _as_data, plus
        assert (first["line"], first["needs_you"], first["actions"]) == ("Waiting on your Go", True, ["go", "drop"])
        assert (first["steps"], first["steps_done"], first["rev"]) == (3, 2, 1)
        clock.t += 60
        assert (await act(port, token, "c2", "drop"))[1]["ok"]
        clock.t += 60
        assert (await act(port, token, "c3", "go"))[1]["ok"]          # approved, not yet run: no longer needs you
        got = await jobs(port, token)
        assert [j["id"] for j in got["jobs"]] == ["c1", "c3", "c2"] and got["needs_you"] == 1
        c3, c2 = got["jobs"][1], got["jobs"][2]
        assert (c3["needs_you"], c3["actions"]) == (False, ["drop"])
        assert (c2["line"], c2["needs_you"], c2["actions"]) == ("Dropped", False, ["hide"])
        inlet.chief = None
        status, _, body = await http(port, "GET", link_chat.JOBS_PATH, token=token)
        assert status == 503 and json.loads(body)["error"] == link_chat.NO_CHIEF
        await server.close()

    asyncio.run(go())


def test_a_finished_job_can_be_removed_from_work_for_good(tmp_path: Path) -> None:
    """Owner, 2026-10-10: "i should also have ability to delete completed jobs i dont want to be forced to see
    them". A finished card leaves the list, stays gone after a restart, and stays in the chief's ledger."""
    c, f, _ = chief_at(tmp_path)

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet)
        assert (await act(port, token, "c2", "hide"))[1] == {"ok": False, "reason": "c2 is still open; drop it first"}
        await act(port, token, "c2", "drop")
        assert "c2" in [j["id"] for j in (await jobs(port, token))["jobs"]]
        assert (await act(port, token, "c2", "hide"))[1] == {"ok": True, "hidden": "c2"}
        assert "c2" not in [j["id"] for j in (await jobs(port, token))["jobs"]]
        assert c.ledger.get("c2").status == "dropped"                              # the chief's record is whole
        assert (await act(port, token, "c99", "hide"))[0] == 404
        await server.close()

    asyncio.run(go())
    again = link_chat.HiddenJobs(tmp_path / "hidden-jobs.json")                    # a restart reads it back
    assert again.ids == {"c2"} and "c2" not in [j["id"] for j in link_chat.list_jobs(c, again.ids)["jobs"]]
    assert (tmp_path / "hidden-jobs.json").stat().st_mode & 0o777 == 0o600
    (tmp_path / "bad.json").write_text("{not json")
    assert link_chat.HiddenJobs(tmp_path / "bad.json").ids == set()                # unreadable: all shown again


def test_only_the_last_twenty_finished_cards_are_listed(tmp_path: Path) -> None:
    f, clock = Fakes(), Clock(chief_clock(10))
    c = make(tmp_path / "chief", f, clock)

    async def many() -> None:
        for _ in range(23):
            out = await take(c)
            clock.t += 1
            c.drop(out["id"])

    asyncio.run(many())
    got = link_chat.list_jobs(c)
    assert len(got["jobs"]) == link_chat.FINISHED_SHOWN == 20 and got["needs_you"] == 0
    assert got["jobs"][0]["id"] == "c23" and got["jobs"][-1]["id"] == "c4"      # the newest finished, newest first


def test_each_action_is_the_chiefs_own_method(tmp_path: Path) -> None:
    c, f, _ = chief_at(tmp_path)

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet)
        status, r = await act(port, token, "c1", "go")
        assert status == 200 and r == {"ok": True, "id": "c1", "phase": 3}
        approved = [e for e in c.ledger.events("c1") if e["e"] == "approved"]
        assert len(approved) == 1 and approved[0]["via"] == "id"                     # as "go c1" in Telegram
        assert c.ledger.get("c1").phase(3).approval is not None
        assert (await act(port, token, "c2", "raise"))[1] == {"ok": True, "id": "c2", "usd": c.ledger.get("c2").budget.usd,
                                                             "minutes": c.ledger.get("c2").budget.wall_min}
        assert c.ledger.get("c2").budget.usd == 2 * c.ledger.get("c3").budget.usd
        assert (await act(port, token, "c2", "drop"))[1] == {"ok": True, "id": "c2"}
        assert c.ledger.get("c2").status == "dropped"
        assert (await act(port, token, "c2", "drop"))[1]["ok"] is False             # the chief's own refusal
        assert (await act(port, token, "c3", "done"))[1] == {"ok": True, "id": "c3"}
        assert c.ledger.get("c3").status == "done"
        assert any(t == "Done: Standing desk (your call)." for t, _ in f.sent)
        status, r = await act(port, token, "c99", "go")
        assert status == 404 and r["ok"] is False
        assert (await http(port, "POST", f"{link_chat.JOBS_PATH}/c1/delete", token=token))[0] == 404
        assert (await http(port, "GET", f"{link_chat.JOBS_PATH}/c1/go", token=token))[0] == 405
        assert (await http(port, "POST", link_chat.JOBS_PATH, {"x": 1}, token=token))[0] == 400   # New job: no goal
        assert (await http(port, "PUT", link_chat.JOBS_PATH, token=token))[0] == 405
        assert (await act(port, token, "c1", "go", {"rev": "1"}))[0] == 400
        assert (await act(port, token, "c1", "go", {"rev": True}))[0] == 400
        assert (await act(port, token, "c1", "go", b"[1]"))[0] == 400
        await server.close()

    asyncio.run(go())
    asyncio.run(c.drain())
    assert [t["card_id"] for t in f.tasks] == ["c1"]                                  # the app's Go ran c1's act


def test_a_go_from_the_app_keeps_the_chiefs_rules(tmp_path: Path) -> None:
    c, f, clock = chief_at(tmp_path)

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet)
        assert c.change("c1", {"never": ["over $550"]}, said="keep it under $550")["rev"] == 2
        status, r = await act(port, token, "c1", "go", {"rev": 1})                    # the card the app showed
        assert status == 200 and r["ok"] is False and "changed" in r["reason"]
        assert c.ledger.get("c1").phase(3).approval is None
        assert (await jobs(port, token))["jobs"][0]["rev"] == 2
        assert (await act(port, token, "c1", "go", {"rev": 2}))[1]["ok"]              # positive control
        clock.t = chief_clock(23)                                                              # quiet hours
        assert (await act(port, token, "c2", "go", {"rev": 1}))[1]["ok"]              # recorded, as in Telegram
        await server.close()

    asyncio.run(go())
    asyncio.run(c.drain())
    assert [t["card_id"] for t in f.tasks] == [] and dispatched(c, "c2", 3) == []     # nothing acts at 23:00
    clock.t = chief_clock(8, 5, day=30)
    asyncio.run(c.drain())
    assert sorted(t["card_id"] for t in f.tasks) == ["c1", "c2"]


def doubt(c: Any, cid: str, n: int) -> None:
    """The act was dispatched and its result will never come (a restart): "I may have done …"."""
    assert c.ledger.event("dispatched", cid, phase=n)
    asyncio.run(c._doubt(c.ledger.get(cid), c.ledger.get(cid).phase(n), "restarted"))


def test_an_act_that_may_have_happened_gets_no_go_only_the_question(tmp_path: Path) -> None:
    c, f, _ = chief_at(tmp_path)
    doubt(c, "c1", 3)
    doubt(c, "c2", 3)
    assert c._doubtful(c.ledger.get("c1")) is not None
    assert [w for _, w in c.choices("c1")] == ["happened c1", "didnt c1"]               # Telegram: no Go button

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet)
        first = (await jobs(port, token))["jobs"][0]
        assert first["id"] == "c1" and first["line"] == "May have happened: check" and first["needs_you"]
        assert first["actions"] == ["happened", "didnt", "drop"]                       # never "go"
        status, r = await act(port, token, "c1", "go")
        assert status == 200 and r["ok"] is False and "may already have done" in r["reason"]
        status, r = await act(port, token, "c1", "go", {"rev": c.ledger.get("c1").rev})
        assert r["ok"] is False
        assert c.ledger.get("c1").phase(3).approval is None                           # nothing approved
        assert [e for e in c.ledger.events("c1") if e["e"] == "approved"] == []
        assert (await act(port, token, "c1", "didnt"))[1]["ok"]                      # it didn't: a new Go is asked
        assert c._doubtful(c.ledger.get("c1")) is None
        after = (await jobs(port, token))["jobs"]
        assert next(j for j in after if j["id"] == "c1")["actions"] == ["go", "drop"]
        assert (await act(port, token, "c1", "go"))[1]["ok"]                         # positive control: now it may
        assert (await act(port, token, "c2", "happened"))[1]["ok"]                   # it happened: taken as done
        assert c.ledger.get("c2").phase(3).status == "ok" and c._doubtful(c.ledger.get("c2")) is None
        assert (await act(port, token, "c3", "happened"))[1]["ok"] is False           # c3 has nothing in doubt
        await server.close()

    asyncio.run(go())


# ---- New job: starting a card from the Work tab ----------------------------------------------------------------

GOAL = "find me a standing desk under $600 and order it"


async def new_job(port: int, token: str, body: Any) -> tuple[int, dict[str, Any]]:
    status, _, raw = await http(port, "POST", link_chat.JOBS_PATH, body, token=token)
    return status, json.loads(raw)


def take_on_script(args: dict[str, Any] = DESK_JOB, *, reply: bool = True) -> Callable[[FakeInlet, str], Any]:
    """The turn as the brain would take it: the chief's own take_on with the words heard, then its backbrief."""

    async def script(inlet: FakeInlet, app: str) -> None:
        await asyncio.sleep(0.01)
        words = inlet.heard[-1][0]
        out = await inlet.chief.handle("take_on", args, said=words, said_ref="2026-09-29 10:00:00")
        if out.get("ok"):
            inlet.filed.setdefault(app, out["id"])           # as TelegramInlet._chief_tool records it
        if reply:
            inlet.out(app, out["backbrief"] if out.get("ok") else "Which desk do you mean?")
        inlet.busy = False

    return script


def fresh_chief(tmp_path: Path) -> tuple[Any, Fakes]:
    f = Fakes()
    return make(tmp_path / "chief", f, Clock(chief_clock(10))), f


def test_the_job_lead_names_no_act() -> None:
    from eve_bridge import chief_card

    assert chief_card.verbs(link_chat.JOB_LEAD + link_chat.JOB_DETAIL_LEAD) == frozenset()
    assert chief_card.verbs("order it") == frozenset({"order"})                     # positive control


def test_a_new_job_is_heard_as_an_app_turn_and_answers_the_card_it_filed(tmp_path: Path) -> None:
    c, _ = fresh_chief(tmp_path)

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), script=take_on_script())
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01)
        status, got = await new_job(port, token, {"goal": f"  {GOAL} ", "detail": "  black, 120 cm  "})
        assert status == 200 and got["ok"] is True, got
        [(heard, on_call, app)] = inlet.heard
        assert heard == f"{link_chat.JOB_LEAD}{GOAL}\n\n{link_chat.JOB_DETAIL_LEAD}black, 120 cm"
        assert on_call is False and app                                    # typed, and an app turn's
        card = c.ledger.get("c1")
        assert got["job"] == link_chat.job_entry(c, card)                    # GET's shape for one job
        assert got["job"]["id"] == "c1" and got["job"]["title"] == "Standing desk"
        assert got["said"][0].startswith("On it: Standing desk.") and inlet.delivered == [True]   # read here
        # the chief's own rules: the act is grounded by the owner's word, and it still waits on a Go
        assert card.phase(3).door == "one_way" and card.phase(3).grounded and card.phase(3).approval is None
        listed = await jobs(port, token)
        assert [j["id"] for j in listed["jobs"]] == ["c1"]
        await server.close()

    asyncio.run(go())


def test_a_new_job_the_brain_does_not_file_says_so_and_files_nothing(tmp_path: Path) -> None:
    c, _ = fresh_chief(tmp_path)
    bad = {**DESK_JOB, "kind": "bogus"}                                      # chief_card.parse refuses it

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), script=take_on_script(bad))
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01)
        status, got = await new_job(port, token, {"goal": GOAL})
        assert status == 200 and got == {"ok": False, "said": ["Which desk do you mean?"],
                                          "reason": "eve answered without starting a job"}
        assert c.ledger.all() == [] and len(inlet.heard) == 1
        await server.close()

    asyncio.run(go())


def test_a_new_job_retried_with_its_client_id_is_heard_once(tmp_path: Path) -> None:
    c, _ = fresh_chief(tmp_path)

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), script=take_on_script())
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01)
        body = {"goal": GOAL, "client_id": "job-1234abcd"}
        first, second = await asyncio.gather(new_job(port, token, body), new_job(port, token, body))
        assert first[1]["job"]["id"] == second[1]["job"]["id"] == "c1"
        third = await new_job(port, token, body)                             # after it finished: the same card
        assert third[1]["ok"] and third[1]["job"]["id"] == "c1"
        assert len(inlet.heard) == 1 and [x.id for x in c.ledger.all()] == ["c1"]
        other = await new_job(port, token, {"goal": GOAL, "client_id": "job-5678efgh"})   # positive control
        assert other[1]["job"]["id"] == "c2" and len(inlet.heard) == 2
        await server.close()

    asyncio.run(go())


def test_a_new_job_answers_the_card_its_own_turn_filed_not_a_newer_one(tmp_path: Path) -> None:
    """Gap 2 (2026-10-09): while the New job's turn runs, another turn (a chat turn) files a card after it. The
    endpoint answers the card its own turn's take_on filed, never the newest."""
    f, clock = Fakes(), Clock(chief_clock(10))
    c = make(tmp_path / "chief", f, clock)
    other_job = {**DESK_JOB, "title": "Dentist", "end_state": "A dentist visit is booked."}

    async def script(inlet: FakeInlet, app: str) -> None:
        await asyncio.sleep(0.01)
        words = inlet.heard[-1][0]
        out = await inlet.chief.handle("take_on", DESK_JOB, said=words, said_ref="2026-09-29 10:00:00")
        inlet.filed.setdefault(app, out["id"])
        clock.t += 60                                       # a minute later, another turn files its own card
        other = await inlet.chief.handle("take_on", other_job, said="book the dentist and order it",
                                         said_ref="2026-09-29 10:01:00")
        inlet.filed.setdefault("another-turn-0001", other["id"])
        await asyncio.sleep(0.05)
        inlet.out(app, out["backbrief"])
        inlet.busy = False

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), script=script)
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01)
        status, got = await new_job(port, token, {"goal": GOAL})
        assert status == 200 and got["ok"] is True, got
        assert [x.id for x in c.ledger.all()] == ["c1", "c2"]                    # two cards, c2 the newer
        assert c.ledger.get("c2").created > c.ledger.get("c1").created
        assert got["job"]["id"] == "c1" and got["job"]["title"] == "Standing desk"   # its own, not the newest
        await server.close()

    asyncio.run(go())


def test_a_new_job_whose_turn_filed_nothing_is_not_handed_another_turns_card(tmp_path: Path) -> None:
    """The New job's turn files no card while another turn files one: the answer is the no-card answer."""
    c, _ = fresh_chief(tmp_path)

    async def script(inlet: FakeInlet, app: str) -> None:
        await asyncio.sleep(0.01)
        other = await inlet.chief.handle("take_on", DESK_JOB, said=GOAL, said_ref="2026-09-29 10:00:00")
        inlet.filed.setdefault("another-turn-0001", other["id"])
        inlet.out(app, "Which desk do you mean?")
        inlet.busy = False

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), script=script)
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01)
        status, got = await new_job(port, token, {"goal": GOAL})
        assert status == 200 and got == {"ok": False, "said": ["Which desk do you mean?"],
                                          "reason": "eve answered without starting a job"}
        assert [x.id for x in c.ledger.all()] == ["c1"]                          # positive control: it was filed
        await server.close()

    asyncio.run(go())


def test_a_new_job_still_running_at_its_limit_says_so(tmp_path: Path) -> None:
    c, _ = fresh_chief(tmp_path)

    async def never(inlet: FakeInlet, app: str) -> None:
        await asyncio.sleep(30)

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), script=never)
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01, job_secs=0.2)
        status, got = await new_job(port, token, {"goal": GOAL})
        assert status == 200 and got["ok"] is False and "still on it" in got["reason"]
        await server.close()

    asyncio.run(go())


@pytest.mark.parametrize("body,status", [
    ({}, 400),
    ({"goal": ""}, 400),
    ({"goal": "   "}, 400),
    ({"goal": 5}, 400),
    ({"goal": "x" * (link_chat.MAX_GOAL_CHARS + 1)}, 400),
    ({"goal": "ok", "detail": "x" * (link_chat.MAX_DETAIL_CHARS + 1)}, 400),
    ({"goal": "ok", "detail": ["no"]}, 400),
    ({"goal": "ok", "client_id": "x" * (link_chat.MAX_CLIENT_ID_CHARS + 1)}, 400),
    (b"[1]", 400),
    (b"not json", 400),
    ({"goal": "x" * (link_chat.MAX_BODY_BYTES + 10)}, 413),
])
def test_the_limits_on_a_new_job(tmp_path: Path, body: Any, status: int) -> None:
    c, _ = fresh_chief(tmp_path)

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), script=take_on_script())
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet)
        assert (await http(port, "POST", link_chat.JOBS_PATH, body, token=token))[0] == status
        assert inlet.heard == [] and c.ledger.all() == []
        await server.close()

    asyncio.run(go())


def test_a_new_job_needs_the_token_the_chief_and_the_chat(tmp_path: Path) -> None:
    c, _ = fresh_chief(tmp_path)

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), script=take_on_script())
        inlet.chief = c
        server, port, token = await serve(tmp_path, inlet, settle_secs=0.01)
        body = {"goal": GOAL}
        assert (await http(port, "POST", link_chat.JOBS_PATH, body))[0] == 401
        assert (await http(port, "POST", link_chat.JOBS_PATH, body, token=token[:-1]))[0] == 401
        assert (await http(port, "POST", link_chat.JOBS_PATH, body, token=token, origin="null"))[0] == 403
        inlet.chief = None
        assert await new_job(port, token, body) == (503, {"ok": False, "error": link_chat.NO_CHIEF})
        inlet.chief, inlet._chat_id = c, None
        assert await new_job(port, token, body) == (503, {"ok": False, "error": link_chat.NO_CHAT})
        assert inlet.heard == [] and c.ledger.all() == []
        inlet._chat_id = OWNER
        assert (await new_job(port, token, body))[1]["ok"]                    # positive control
        await server.close()

    asyncio.run(go())


def test_a_new_job_through_the_real_chat_is_the_brains_take_on(tmp_path: Path,
                                                                 monkeypatch: pytest.MonkeyPatch) -> None:
    """The real TelegramInlet and a real Chief: one model call, its take_on filed by the chief's own handle with
    its door floor, the same tools and instructions as any chat turn, and the backbrief sent as take_on's always is."""
    from test_telegram_chief import CLOCK_NOTE
    from test_telegram_chief import Rig as ChiefRig

    from eve_bridge import chief, system_context

    monkeypatch.setattr(system_context, "context", lambda: CLOCK_NOTE)
    monkeypatch.delenv("EVE_CHIEF", raising=False)
    rig = ChiefRig(tmp_path, FakeApi(), FakeCreate(call("take_on", DESK_JOB)), with_chief=True)
    rig.inlet._chat_id = OWNER

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        status, got = await new_job(port, token, {"goal": GOAL})
        assert status == 200 and got["ok"], got
        assert len(rig.create.requests) == 1                                 # take_on ends the turn
        body = rig.create.requests[0]
        assert "take_on" in {t.get("name") for t in body["tools"]}
        assert chief.instructions() in body["instructions"]
        assert any(f"{link_chat.JOB_LEAD}{GOAL}" in json.dumps(item) for item in body["input"])
        card = rig.chief.ledger.get("c1")
        assert got["job"] == link_chat.job_entry(rig.chief, card)
        assert card.phase(3).door == "one_way" and card.phase(3).grounded and card.phase(3).approval is None
        # the card is the one this turn's take_on filed, recorded against the app turn's id (gap 2)
        [(turn, cid)] = rig.inlet._app_filed.items()
        assert cid == "c1" and rig.inlet.app_filed(turn) == "c1" and rig.inlet.app_filed("another") is None
        # the backbrief goes through tell_owner as every take_on's does, and, the owner being in the app, to the
        # app with its Change and Drop as the words that answer them; nothing went to Telegram (gap 1)
        assert rig.api.sent == [] and rig.api.buttons == []
        [said] = got["said"]
        assert said.startswith(f"**{telegram.CHIEF_TITLE}**\n\nOn it: Standing desk.")
        assert said.endswith('Reply "change c1" or "drop c1".')
        await server.close()

    asyncio.run(go())
    telegram.unhide_tokens()


# ---- the owner's apps ---------------------------------------------------------------------------------------

def test_apps_are_listed_and_open_with_a_fresh_app_token(tmp_path: Path) -> None:
    store = AppStore(tmp_path / "apps")
    store.save("<title>Tip jar</title><p>tips</p>", "a tip jar", slug="tip-jar")
    store.save("<title>Plant log</title><p>plants</p>", "a plant log", slug="plant-log")

    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)), store=store)
        status, _, body = await http(port, "GET", link_chat.APPS_PATH, token=token)
        listed = json.loads(body)["apps"]
        assert status == 200 and sorted(a["slug"] for a in listed) == ["plant-log", "tip-jar"]
        assert {a["slug"]: a["name"] for a in listed} == {"tip-jar": "Tip jar", "plant-log": "Plant log"}
        assert all(set(a) == {"slug", "name", "updated"} and a["updated"] for a in listed)
        status, _, body = await http(port, "POST", f"{link_chat.APPS_PATH}/tip-jar/open", token=token)
        url = json.loads(body)["url"]
        prefix = "http://127.0.0.1/apps/tip-jar/?t="
        assert status == 200 and url.startswith(prefix)
        t = url[len(prefix):]
        assert check_app_token(t, "tip-jar", TG_TOKEN) and not check_app_token(t, "plant-log", TG_TOKEN)
        page = await http(port, "GET", f"/apps/tip-jar/?t={t}")
        assert page[0] == 200 and b"tips" in page[2]                                   # the page it names opens
        server.public_url = "https://example.trycloudflare.com"
        body = (await http(port, "POST", f"{link_chat.APPS_PATH}/tip-jar/open", token=token))[2]
        assert json.loads(body)["url"].startswith("https://example.trycloudflare.com/apps/tip-jar/?t=")
        for slug in ("nope", "..", "tip.jar"):
            assert (await http(port, "POST", f"{link_chat.APPS_PATH}/{slug}/open", token=token))[0] == 404, slug
        assert (await http(port, "GET", f"{link_chat.APPS_PATH}/tip-jar/open", token=token))[0] == 405
        server.store = None
        status, _, body = await http(port, "GET", link_chat.APPS_PATH, token=token)
        assert status == 503 and json.loads(body)["error"] == link_chat.NO_APPS
        await server.close()

    asyncio.run(go())


# ---- the controls: lights, music, the TV remote ------------------------------------------------------------

class FakePanel:
    """A card's ``panel(body)`` (lights.Lights, spotify.Spotify, firetv.FireTV): records each body, answers a
    fixed dict. ``listing`` is the lights' (names and rooms)."""

    def __init__(self, answer: dict[str, Any], listing: Optional[list[dict[str, str]]] = None) -> None:
        self.answer = answer
        self.bodies: list[dict[str, Any]] = []
        self._listing = listing or []

    async def panel(self, body: dict[str, Any]) -> dict[str, Any]:
        self.bodies.append(body)
        return self.answer

    def listing(self) -> list[dict[str, str]]:
        return self._listing


def test_controls_need_the_link_token_and_no_origin(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        tv = FakePanel({"ok": True, "line": "Home."})
        server.firetv = tv
        doors = (("GET", link_chat.CONTROLS_PATH), ("POST", f"{link_chat.CONTROLS_PATH}/firetv"))
        for method, path in doors:
            assert (await http(port, method, path, {"action": "home"}))[0] == 401, path
            assert (await http(port, method, path, {"action": "home"}, token=token[:-1]))[0] == 401, path
            assert (await http(port, method, path, {"action": "home"}, token=token,
                               origin="https://evil.example"))[0] == 403, path
            assert (await http(port, method, path, {"action": "home"}, token=token, origin="null"))[0] == 403, path
        assert tv.bodies == []                                                       # nothing was pressed
        for method, path in doors:                                                   # positive controls
            assert (await http(port, method, path, {"action": "home"}, token=token))[0] == 200, path
        assert tv.bodies == [{"action": "home"}]
        await server.close()

    asyncio.run(go())


def test_controls_list_what_this_mac_has(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        status, _, body = await http(port, "GET", link_chat.CONTROLS_PATH, token=token)
        assert status == 200 and json.loads(body) == {"lights": [], "spotify": False, "firetv": False, "memory": False,
                                                       "notes": False, "pika": False, "obsbot": False, "apps": []}
        server.lights = FakePanel({}, listing=[{"name": "desk lamp", "room": "office"}, {"name": "strip"}])
        server.spotify = FakePanel({})
        status, _, body = await http(port, "GET", link_chat.CONTROLS_PATH, token=token)
        assert status == 200 and json.loads(body) == {
            "lights": [{"name": "desk lamp", "room": "office"}, {"name": "strip"}], "spotify": True, "firetv": False,
            "memory": False, "notes": False, "pika": False, "obsbot": False,
            # the phone's Apps list (app_registry.for_phone): only what is here, in the phone's order
            "apps": [{"id": "spotify", "title": "Music", "symbol": "music.note", "detail": "Spotify", "sheet": "native"},
                     {"id": "lights", "title": "Lights", "symbol": "lightbulb", "detail": "2 lights",
                      "sheet": "native"}]}
        # the Memory card's object is always there (link_memory.py); it is listed only while its memory is built
        from eve_bridge.link_memory import MemoryPanel

        server.memory = MemoryPanel(lambda: None)
        assert json.loads((await http(port, "GET", link_chat.CONTROLS_PATH, token=token))[2])["memory"] is False
        assert (await http(port, "POST", f"{link_chat.CONTROLS_PATH}/memory", {"action": "profile"}, token=token))[0] == 404
        server.memory = MemoryPanel(lambda: object())
        assert json.loads((await http(port, "GET", link_chat.CONTROLS_PATH, token=token))[2])["memory"] is True
        # Notes (link_notes.py): listed only while the second brain is on and its vault folder is there
        from eve_bridge import second_brain
        from eve_bridge.link_notes import NotesPanel

        server.notes = NotesPanel(lambda: second_brain.VaultConfig(enabled=False, root=tmp_path))
        assert json.loads((await http(port, "GET", link_chat.CONTROLS_PATH, token=token))[2])["notes"] is False
        assert (await http(port, "POST", f"{link_chat.CONTROLS_PATH}/notes", {"action": "list"}, token=token))[0] == 404
        server.notes = NotesPanel(lambda: second_brain.VaultConfig(enabled=True, root=tmp_path / "missing"))
        assert json.loads((await http(port, "GET", link_chat.CONTROLS_PATH, token=token))[2])["notes"] is False
        server.notes = NotesPanel(lambda: second_brain.VaultConfig(enabled=True, root=tmp_path))
        assert json.loads((await http(port, "GET", link_chat.CONTROLS_PATH, token=token))[2])["notes"] is True
        assert (await http(port, "POST", link_chat.CONTROLS_PATH, {}, token=token))[0] == 405
        await server.close()

    asyncio.run(go())


def test_an_unknown_control_or_a_missing_device_is_a_404(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        for name in ("stick", "LIGHTS", "lights/x", "apps", "../firetv"):
            assert (await http(port, "POST", f"{link_chat.CONTROLS_PATH}/{name}", {}, token=token))[0] == 404, name
        for name in link_chat.CONTROLS:                                              # none on this Mac
            status, _, body = await http(port, "POST", f"{link_chat.CONTROLS_PATH}/{name}", {"action": "status"},
                                         token=token)
            assert status == 404 and json.loads(body) == {"ok": False, "line": link_chat.NO_CONTROL[name]}, name
        server.lights = FakePanel({"ok": True})
        assert (await http(port, "GET", f"{link_chat.CONTROLS_PATH}/lights", token=token))[0] == 405
        await server.close()

    asyncio.run(go())


def test_a_control_is_the_cards_own_panel_body_in_json_out(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        answers = {"lights": {"ok": True, "line": "Desk lamp is red.", "failed": []},
                   "memory": {"ok": True, "hits": [], "recalled": ["(2026-10-01, app) likes tea"], "said": [], "line": ""},
                   "notes": {"ok": True, "notes": [], "folders": ["01-inbox"], "line": ""},
                   "spotify": {"ok": True, "playing": True, "track": "A song", "artists": ["Someone"],
                               "art": "https://i.scdn.co/image/x", "line": "Playing."},
                   "firetv": {"ok": True, "power": "on", "volume": 40, "muted": False, "now": "TV on, volume 40%",
                              "line": "Volume 40%.", "apps": ["YouTube"]},
                   "pika": {"ok": True, "line": ""},
                   "obsbot": {"ok": True, "line": "Zoom 40%.", "zoom": 40}}
        bodies = {"pika": {"action": "drive", "dir": "forward", "hold": "h1"},
                  "obsbot": {"action": "zoom", "level": 40},
                  "lights": {"action": "set", "target": "desk lamp", "color": "#ff0000", "brightness": 60},
                  "memory": {"action": "search", "query": "tea"},
                  "notes": {"action": "list"},
                  "spotify": {"action": "volume", "volume": 30},
                  "firetv": {"action": "volume_change", "change": 5}}
        for name in link_chat.CONTROLS:                         # an app added since: its generic sheet's status read
            answers.setdefault(name, {"ok": True, "ui": 1, "status": "On"})
            bodies.setdefault(name, {"action": "status"})
        panels = {n: FakePanel(a) for n, a in answers.items()}
        for name, panel in panels.items():
            setattr(server, name, panel)
        for name in link_chat.CONTROLS:
            status, _, body = await http(port, "POST", f"{link_chat.CONTROLS_PATH}/{name}", bodies[name],
                                         token=token)
            assert status == 200 and json.loads(body) == answers[name], name
            assert panels[name].bodies == [bodies[name]], name
        status, _, body = await http(port, "POST", f"{link_chat.CONTROLS_PATH}/firetv", b"", token=token)
        assert status == 200 and panels["firetv"].bodies[-1] == {}                  # no body is a status read
        assert (await http(port, "POST", f"{link_chat.CONTROLS_PATH}/firetv", [1], token=token))[0] == 400
        assert (await http(port, "POST", f"{link_chat.CONTROLS_PATH}/firetv", {"x": "y" * 70_000},
                           token=token))[0] == 413
        assert len(panels["firetv"].bodies) == 2                                     # refused bodies never reach it

        class Broken:
            async def panel(self, body: dict[str, Any]) -> dict[str, Any]:
                raise RuntimeError("the bulb said no")
        server.lights = Broken()
        status, _, body = await http(port, "POST", f"{link_chat.CONTROLS_PATH}/lights", {"action": "status"},
                                     token=token)
        assert status == 500 and json.loads(body)["ok"] is False
        await server.close()

    asyncio.run(go())


def test_a_bare_program_after_prose_is_stripped_like_the_app_draws_it() -> None:
    """replyParts.ts draws `root = Card(` at any line start (no ``` before it); history must strip it the same way."""
    text = "Here you go:\nroot = Card([t])\nt = TextContent(\"hi\")"
    assert link_chat.strip_openui(text) == "Here you go:\n" + link_chat.OPENUI_PLACEHOLDER
    assert link_chat.strip_openui("root = 5 is not a program") == "root = 5 is not a program"
    assert link_chat.strip_openui("prose\nroot = Card([x])") .endswith(link_chat.OPENUI_PLACEHOLDER)


# ---- Updates (updates.py): the pane left of Chat --------------------------------------------------------------

class SlowUpdatesClient:
    """The suggester's model, slow enough to see a refresh in flight; one Pay card citing the first item."""

    def __init__(self) -> None:
        self.calls = 0

    def complete(self, instructions: str, body: str) -> str:
        self.calls += 1
        time.sleep(0.3)
        ref = json.loads(body.split("<snapshot>\n", 1)[1].rsplit("\n</snapshot>", 1)[0])["items"][0]["id"]
        return json.dumps({"cards": [{"verb": "Pay", "text": "the water bill of $64 by Oct 20", "entity": "City Water",
                                      "refs": [ref], "due": "2026-10-20"}]})


def updates_at(tmp_path: Path, client: Any = None, env: Optional[dict[str, str]] = None) -> Any:
    from eve_bridge.updates import Sources, Updates

    class OneNote(Sources):
        def apps(self, app: str) -> Any:
            return None

        def vault(self) -> Optional[Path]:
            root = tmp_path / "vault"
            root.mkdir(exist_ok=True)
            (root / "bills.md").write_text("# Bills\n- [ ] Pay the water bill by Oct 20\n")
            return root

        def chief(self) -> Any:
            return None

    return Updates(path=tmp_path / "updates.json", sources=OneNote(records_dir=lambda: None, hidden=lambda: None),
                   client=lambda: client, environ=env if env is not None else {})


def test_updates_need_the_link_token_and_no_origin(tmp_path: Path) -> None:
    async def go() -> None:
        u = updates_at(tmp_path, SlowUpdatesClient())
        u.store.put_cards([{"id": "abc123abc123", "verb": "Pay", "text": "x", "refs": ["note:bills.md"],
                            "kind": "note", "source": "bills"}], "2026-10-12T09:00:00-07:00")
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)), updates=u)
        doors = (("GET", link_chat.UPDATES_PATH, None), ("POST", link_chat.UPDATES_DISMISS_PATH, {"id": "abc123abc123"}),
                 ("POST", link_chat.UPDATES_REFRESH_PATH, {}))
        for method, path, body in doors:
            assert (await http(port, method, path, body))[0] == 401, path
            assert (await http(port, method, path, body, token=token[:-1]))[0] == 401, path
            assert (await http(port, method, path, body, token=token, origin="https://evil.example"))[0] == 403, path
            assert (await http(port, method, path, body, token=token, origin="null"))[0] == 403, path
        assert not u.refreshing and u.store.load()["dismissed"] == {}                  # nothing was done
        assert (await http(port, "POST", link_chat.UPDATES_PATH, {}, token=token))[0] == 405
        assert (await http(port, "GET", link_chat.UPDATES_REFRESH_PATH, token=token))[0] == 405
        for method, path, body in doors:                                              # positive controls
            assert (await http(port, method, path, body, token=token))[0] == 200, path
        assert u.store.load()["dismissed"] == {"note:bills.md": u._now().date().isoformat()}
        if u._task is not None:
            await u._task
        await server.close()

    asyncio.run(go())


def test_updates_serve_the_contract_dismiss_and_refresh_without_waiting(tmp_path: Path) -> None:
    async def go() -> None:
        client = SlowUpdatesClient()
        u = updates_at(tmp_path, client)
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)), updates=u)
        status, _, body = await http(port, "GET", link_chat.UPDATES_PATH, token=token)
        assert status == 200 and json.loads(body) == {"cards": [], "generated_at": None, "refreshing": False}

        started = time.monotonic()
        status, _, body = await http(port, "POST", link_chat.UPDATES_REFRESH_PATH, {}, token=token)
        assert status == 200 and json.loads(body) == {"ok": True, "started": True}
        assert time.monotonic() - started < 0.25                                     # it never waits on the model
        status, _, body = await http(port, "POST", link_chat.UPDATES_REFRESH_PATH, b"", token=token)
        assert json.loads(body) == {"ok": True, "started": False}                    # one at a time
        assert json.loads((await http(port, "GET", link_chat.UPDATES_PATH, token=token))[2])["refreshing"] is True
        await u._task
        view = json.loads((await http(port, "GET", link_chat.UPDATES_PATH, token=token))[2])
        assert view["refreshing"] is False and view["generated_at"] and client.calls == 1
        [card] = view["cards"]
        assert set(card) == {"id", "verb", "text", "entity", "kind", "source", "due"}
        assert (card["verb"], card["kind"], card["source"], card["due"]) == ("Pay", "note", "bills", "2026-10-20")

        for bad in ({}, {"id": 5}, {"id": ""}, {"id": "x" * 65}, [1]):
            assert (await http(port, "POST", link_chat.UPDATES_DISMISS_PATH, bad, token=token))[0] == 400, bad
        status, _, body = await http(port, "POST", link_chat.UPDATES_DISMISS_PATH, {"id": "feedfeedfeed"}, token=token)
        assert status == 404 and json.loads(body)["ok"] is False                      # nothing dismissed: said so
        assert u.store.load()["dismissed"] == {}
        status, _, body = await http(port, "POST", link_chat.UPDATES_DISMISS_PATH, {"id": card["id"]}, token=token)
        assert status == 200 and json.loads(body) == {"ok": True}
        assert json.loads((await http(port, "GET", link_chat.UPDATES_PATH, token=token))[2])["cards"] == []
        await server.close()

    asyncio.run(go())


def test_updates_off_answer_empty_and_read_nothing(tmp_path: Path) -> None:
    async def go() -> None:
        client = SlowUpdatesClient()
        u = updates_at(tmp_path, client, env={"EVE_UPDATES": "0"})
        u.store.put_cards([{"id": "abc123abc123", "verb": "Pay", "text": "x", "refs": ["note:bills.md"]}],
                          "2026-10-12T09:00:00-07:00")
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)), updates=u)
        status, _, body = await http(port, "GET", link_chat.UPDATES_PATH, token=token)
        assert status == 200 and json.loads(body) == {"cards": [], "generated_at": None, "refreshing": False}
        status, _, body = await http(port, "POST", link_chat.UPDATES_REFRESH_PATH, {}, token=token)
        assert json.loads(body) == {"ok": True, "started": False} and client.calls == 0
        await server.close()

    asyncio.run(go())


# ---- the phone's location (phone_location.py) -------------------------------------------------------------

LOCATION_FIX = {"lat": 0.0, "lon": 0.0, "accuracy_m": 15.0, "at": "", "place": "Null Island", "speed_mps": None}


def location(minutes_ago: float = 1.0, **kw: Any) -> dict[str, Any]:
    from datetime import timezone

    out = dict(LOCATION_FIX, at=(datetime.now(timezone.utc) - timedelta(minutes=minutes_ago)).isoformat())
    out.update(kw)
    return out


def test_the_phone_location_needs_the_link_token_and_no_origin(tmp_path: Path) -> None:
    from eve_bridge import phone_location

    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        path = link_chat.LOCATION_PATH
        assert (await http(port, "POST", path, location()))[0] == 401                         # no token
        assert (await http(port, "POST", path, location(), token=token[:-1]))[0] == 401        # nearly right
        for origin in ("https://evil.example", "null"):
            assert (await http(port, "POST", path, location(), token=token, origin=origin))[0] == 403
        assert phone_location.load(phone_location.state_path()) is None                       # nothing was kept
        assert (await http(port, "GET", path, token=token))[0] == 405                          # POST only
        status, _, raw = await http(port, "POST", path, location(), token=token)               # positive control
        assert status == 200 and json.loads(raw) == {"ok": True}
        assert phone_location.load(phone_location.state_path()).place == "Null Island"
        await server.close()

    asyncio.run(go())


@pytest.mark.parametrize("bad", [
    b"", b"{not json", b"[1, 2]", location(lat=91), location(lon="0"), location(accuracy_m=-3),
    location(at="2026-10-12T17:55:00"), location(minutes_ago=-30), location(place=5), location(speed_mps="x"),
    {"lat": 0.0, "lon": 0.0}, location(lat=10 ** 400),          # too big for a float: a 400, not a dropped socket
])
def test_a_body_that_is_not_a_fix_is_a_400_and_keeps_nothing(tmp_path: Path, bad: Any) -> None:
    from eve_bridge import phone_location

    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        status, _, raw = await http(port, "POST", link_chat.LOCATION_PATH, bad, token=token)
        assert status == 400 and json.loads(raw)["ok"] is False and json.loads(raw)["error"]
        assert not phone_location.state_path().exists()
        await server.close()

    asyncio.run(go())


def test_a_body_too_big_is_a_413(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        big = location(place="x" * (link_chat.MAX_BODY_BYTES + 1))
        assert (await http(port, "POST", link_chat.LOCATION_PATH, big, token=token))[0] == 413
        await server.close()

    asyncio.run(go())


def test_an_older_fix_is_ignored_and_the_log_never_says_where(tmp_path: Path,
                                                              caplog: pytest.LogCaptureFixture) -> None:
    from eve_bridge import phone_location

    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        new = location(1, lat=37.7749, lon=-122.4194, place="San Francisco, CA")
        assert (await http(port, "POST", link_chat.LOCATION_PATH, new, token=token))[0] == 200
        old = location(10, lat=1.0, lon=1.0, place="Null Island")
        status, _, raw = await http(port, "POST", link_chat.LOCATION_PATH, old, token=token)
        assert status == 200 and json.loads(raw) == {"ok": True}
        await server.close()

    with caplog.at_level(logging.DEBUG):
        asyncio.run(go())
    kept = phone_location.load(phone_location.state_path())
    assert kept is not None and (kept.lat, kept.place) == (37.7749, "San Francisco, CA")
    said = "\n".join(r.getMessage() for r in caplog.records)
    assert "location updated" in said and "location ignored" in said and "accuracy 15 m" in said
    for secret in ("37.77", "122.4", "San Francisco", "Null Island", "1.0, 1.0"):
        assert secret not in said


def test_the_switch_off_is_a_403_and_keeps_nothing(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge import phone_location

    monkeypatch.setenv("EVE_PHONE_LOCATION", "0")

    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        status, _, raw = await http(port, "POST", link_chat.LOCATION_PATH, location(), token=token)
        assert status == 403 and json.loads(raw) == {"ok": False, "error": link_chat.NO_LOCATION}
        assert (await http(port, "POST", link_chat.LOCATION_PATH, location()))[0] == 401   # the token still first
        await server.close()

    asyncio.run(go())
    assert not phone_location.state_path().exists()


def test_a_fix_that_cannot_be_kept_is_a_500(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge import phone_location

    def full(*a: Any) -> bool:
        raise OSError("disk full")

    monkeypatch.setattr(phone_location, "store", full)

    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)))
        assert (await http(port, "POST", link_chat.LOCATION_PATH, location(), token=token))[0] == 500
        await server.close()

    asyncio.run(go())


def test_the_location_route_needs_no_chat_running(tmp_path: Path) -> None:
    from eve_bridge import phone_location

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet._chat_id = None                                    # the chat never started: still kept
        server, port, token = await serve(tmp_path, inlet)
        assert (await http(port, "POST", link_chat.LOCATION_PATH, location(), token=token))[0] == 200
        await server.close()

    asyncio.run(go())
    assert phone_location.load(phone_location.state_path()) is not None


def test_an_app_turn_offers_and_answers_the_phone_location_from_the_fix_it_posted(tmp_path: Path) -> None:
    from eve_bridge import phone_location

    rig, _ = real_rig(tmp_path, call("phone_location", {}), say("You're on Null Island."))

    async def go() -> None:
        server, port, token = await serve(tmp_path, rig.inlet, settle_secs=0.01)
        assert (await http(port, "POST", link_chat.LOCATION_PATH, location(2), token=token))[0] == 200
        status, _, body = await turn(port, token, "where am i right now", "c-loc")
        assert status == 200 and events(body)[0] == ["You're on Null Island."]
        await server.close()

    asyncio.run(go())
    first, second = rig.create.requests[:2]
    assert phone_location.TOOL in first["tools"]
    out = [json.loads(i["output"]) for i in second["input"] if i.get("type") == "function_call_output"]
    assert out[-1]["place"] == "Null Island" and out[-1]["stale"] is False and out[-1]["age_min"] in (1, 2)
