"""eve's expressions in the phone's eyes (owner, 2026-10-12: "route the expressions into the eyes").

The real Mini App server on a loopback port, the Eve Link door (/api/stick), a real WebSocket client as the phone, the
fake chat and voice of test_phone_call, and the real LiveExpressions worker with a fake model (no Jev call)."""
from __future__ import annotations

import asyncio
import json
import threading
import time
from contextlib import suppress
from pathlib import Path
from types import SimpleNamespace
from typing import Any

import pytest
from test_phone_call import OWNER, TOKEN, Brain, Voice, is_, press, until
from websockets.asyncio.client import connect

from eve_bridge import phone_call
from eve_bridge.daemon import Daemon
from eve_bridge.eye_model import LABELS
from eve_bridge.live_expressions import LiveExpressions, phone_expressions_on
from eve_bridge.miniapp import MiniAppConfig, MiniAppServer, SpendLedger, check_init_data

LINK = "link-token-for-tests"
HAPPY = [0, 1] + [0] * (len(LABELS) - 2)


class Model:
    def __init__(self) -> None:
        self.states: list[str] = []

    def predict(self, state):
        self.states.append(state)
        return {"probabilities": HAPPY, "ms": 1.0}


async def serve(tmp_path: Path, brain: Brain | None = None, voice: Voice | None = None):
    cfg = MiniAppConfig(enabled=True, token=TOKEN, owner_ids=frozenset({OWNER}), ledger_path=tmp_path / "l.json")
    server = MiniAppServer(cfg, SpendLedger(cfg.ledger_path), page=b"home")
    server.calls = phone_call.PhoneCalls(lambda d: check_init_data(d, TOKEN, cfg.owner_ids),
                                         brain if brain is not None else Brain("Lovely. That went well."),
                                         voice if voice is not None else Voice())
    server.stick = SimpleNamespace(check=lambda token: token == LINK)
    port = await server.start()
    return server, f"ws://127.0.0.1:{port}/api/stick"


async def hello(ws, can=("notes", "eyes")) -> None:
    await ws.send(json.dumps({"token": LINK, "can": list(can)}))
    await until(ws, is_("state", state="connected"))


def picker(calls: phone_call.PhoneCalls, model_factory=Model, **kw) -> LiveExpressions:
    """The daemon's wiring (Daemon._start_phone_expressions), with a fake model."""
    service = LiveExpressions(calls.send_expression, connected=calls.eyes_live, enabled=True, lazy=True,
                              model_factory=model_factory, **kw)
    calls.on_text = service.offer
    return service


async def stop(task) -> None:
    task.cancel()
    with suppress(asyncio.CancelledError):
        await task


async def eventually(pred, secs: float = 5.0) -> None:
    async with asyncio.timeout(secs):
        while not pred():
            await asyncio.sleep(0.01)


def expressions(msgs: list[Any]) -> list[dict]:
    return [m for m in msgs if isinstance(m, dict) and m.get("type") == "expression"]


async def turn_with_pick(ws) -> list[dict]:
    """One press; then its reply (back to listening) and at least one pick, in whichever order they come."""
    await press(ws)
    seen: list[dict] = []
    listening = False
    while not listening or not seen:
        m = (await until(ws, lambda m: is_("expression")(m) or is_("state", state="listening")(m)))[-1]
        if m["type"] == "expression":
            seen.append(m)
        else:
            listening = True
    return seen


# ---- the turn reaches the picker --------------------------------------------------------------------------------

def test_both_sides_of_a_turn_are_offered_in_order(tmp_path):
    async def go():
        server, url = await serve(tmp_path)
        offered: list[tuple[str, str]] = []
        server.calls.on_text = lambda who, text: offered.append((who, text))
        try:
            async with connect(url) as ws:
                await hello(ws)
                await press(ws)
                got = await until(ws, is_("state", state="listening"))
                saids = [m["text"] for m in got if isinstance(m, dict) and m.get("type") == "said"]
                assert offered[0] == ("user", "what's on my calendar")
                # every sentence read out, as it was captioned, in order (each when the phone starts to play it)
                await eventually(lambda: len(offered) == 1 + len(saids))
                assert offered[1:] == [("assistant", s) for s in saids] and saids
        finally:
            await server.close()

    asyncio.run(go())


def test_a_hello_without_eyes_gets_no_offers_and_no_expressions(tmp_path):
    async def go():
        server, url = await serve(tmp_path)
        offered: list[tuple[str, str]] = []
        server.calls.on_text = lambda who, text: offered.append((who, text))
        try:
            async with connect(url) as ws:
                await hello(ws, can=("notes",))           # an older Eve Link build
                await press(ws)
                got = await until(ws, is_("state", state="listening"))
                assert offered == [] and server.calls.eyes_live() is False
                assert await server.calls.send_expression(
                    {"cmd": "expression", "id": 5, "label": "happy", "ttl_ms": 4000}) is False
                assert expressions(got) == []
        finally:
            await server.close()

    asyncio.run(go())


def test_a_picker_fault_never_breaks_the_call(tmp_path):
    async def go():
        server, url = await serve(tmp_path)

        def broken(who, text):
            raise RuntimeError("picker down")

        server.calls.on_text = broken
        try:
            async with connect(url) as ws:
                await hello(ws)
                await press(ws)
                got = await until(ws, is_("state", state="listening"))
                assert any(isinstance(m, bytes) for m in got)              # eve still spoke
        finally:
            await server.close()

    asyncio.run(go())


# ---- the pick reaches the phone ---------------------------------------------------------------------------------

def test_picks_reach_the_phone_in_the_contract_shape_with_increasing_ids(tmp_path):
    async def go():
        server, url = await serve(tmp_path)
        model = Model()
        service = picker(server.calls, model_factory=lambda: model)
        worker = asyncio.create_task(service.run())
        try:
            async with connect(url) as ws:
                await hello(ws)
                seen = await turn_with_pick(ws) + await turn_with_pick(ws)
                for m in seen:
                    # an assistant pick names the sentence it was made for, so the phone starts it with that sound
                    assert set(m) in ({"type", "id", "label", "ttl_ms"}, {"type", "id", "label", "ttl_ms", "said"})
                    assert m["label"] == "happy" and isinstance(m["id"], int) and m["id"] > 0
                    assert 500 <= m["ttl_ms"] <= 4000
                ids = [m["id"] for m in seen]
                assert len(ids) >= 2 and ids == sorted(ids) and len(set(ids)) == len(ids)
                # the picker saw the conversation, the owner's words and eve's
                assert any("User: what's on my calendar" in s for s in model.states)
                assert any("Eve: " in s for s in model.states)
        finally:
            await stop(worker)
            await server.close()

    asyncio.run(go())


class SlowVoice(Voice):
    """Speech made at once, as the real stream nearly is, but each sentence is half a second of sound."""

    def speak_stream(self, text: str):
        self.spoken.append(text)
        yield b"\x01\x00" * 12000


def test_sentences_are_offered_when_the_phone_plays_them_not_when_they_are_made(tmp_path):
    """Speech is made faster than it plays: the second sentence's offer waits for the first's sound."""
    async def go():
        server, url = await serve(tmp_path, Brain("Bad news, the build failed on the release branch this morning because a test timed out. "
                                        "But I fixed the slow test, reran everything twice, and it is all green now!"),
                                  SlowVoice())
        offered: list[tuple[str, str, float]] = []
        server.calls.on_text = lambda who, text: offered.append((who, text, time.monotonic()))
        try:
            async with connect(url) as ws:
                await hello(ws)
                await press(ws)
                got = await until(ws, is_("state", state="listening"))
                saids = [m["text"] for m in got if isinstance(m, dict) and m.get("type") == "said"]
                assert len(saids) == 2
                await eventually(lambda: len(offered) == 3)
                (_, first, t1), (_, second, t2) = offered[1:]
                assert (first, second) == tuple(saids)
                assert t2 - t1 >= 0.4                      # the first sentence's half second of sound, not ~0
        finally:
            await server.close()

    asyncio.run(go())


class FakeWS:
    def __init__(self) -> None:
        self.closed = False
        self.sent: list[dict] = []

    async def send_json(self, msg) -> None:
        self.sent.append(msg)


def test_an_assistant_pick_names_its_sentence_and_others_do_not():
    async def go():
        ws = FakeWS()
        call = phone_call.Call(ws, Brain(), Voice())
        ids = iter([7, 8])
        call.on_text = lambda who, text: next(ids)
        call._text("assistant", "But it's green now!")
        call._text("user", "did it build")
        assert await call.send_expression({"type": "expression", "id": 7, "label": "happy", "ttl_ms": 3000})
        assert await call.send_expression({"type": "expression", "id": 8, "label": "curious", "ttl_ms": 3000})
        assert await call.send_expression({"type": "expression", "id": 7, "clear": True})
        assert ws.sent == [
            {"type": "expression", "id": 7, "label": "happy", "ttl_ms": 3000, "said": "But it's green now!"},
            {"type": "expression", "id": 8, "label": "curious", "ttl_ms": 3000},
            {"type": "expression", "id": 7, "clear": True},
        ]

    asyncio.run(go())


def test_a_barge_in_drops_the_offers_still_waiting_for_their_sound():
    async def go():
        ws = FakeWS()
        call = phone_call.Call(ws, Brain(), Voice())
        offered: list[str] = []
        call.on_text = lambda who, text: offered.append(text)
        call._sent_audio(phone_call.BYTES_PER_SEC // 5)    # 0.2 s of eve's sound queued on the phone
        call._offer_when_heard("two")
        assert offered == [] and len(call.offers) == 1
        await call.interrupt()                              # the owner pressed: the phone flushed it
        await asyncio.sleep(0.35)
        assert offered == [] and not call.offers and call.play_end == 0.0
        call._offer_when_heard("three")                     # nothing queued: offered at once
        assert offered == ["three"]
        call.reading.cancel()
        with suppress(asyncio.CancelledError):
            await call.reading

    asyncio.run(go())


def test_switching_off_clears_the_face_and_writes_no_file(tmp_path, monkeypatch):
    board_file = tmp_path / "expressions.json"
    monkeypatch.setenv("EVE_EXPRESSIONS_FILE", str(board_file))

    async def go():
        server, url = await serve(tmp_path)
        service = picker(server.calls)
        worker = asyncio.create_task(service.run())
        try:
            async with connect(url) as ws:
                await hello(ws)
                await turn_with_pick(ws)
                await service.set_enabled(False)
                cleared = (await until(ws, lambda m: isinstance(m, dict) and m.get("clear") is True))[-1]
                assert cleared == {"type": "expression", "id": cleared["id"], "clear": True}
                assert service.offer("user", "anything") is None
                assert not board_file.exists()                 # the board's switch file is not the phone's
        finally:
            await stop(worker)
            await server.close()

    asyncio.run(go())


# ---- nothing is sent with no live call --------------------------------------------------------------------------

def test_no_live_call_sends_nothing():
    async def go():
        calls = phone_call.PhoneCalls(lambda d: None, None, None)
        sent: list[dict] = []
        real = calls.send_expression

        async def spy(cmd):
            sent.append(cmd)
            return await real(cmd)

        service = LiveExpressions(spy, connected=calls.eyes_live, enabled=True, lazy=True, model_factory=Model)
        worker = asyncio.create_task(service.run())
        try:
            assert service.offer("assistant", "Great news, it shipped!") is not None
            await eventually(lambda: service.dropped == 1)
            assert sent == [] and service.sent == 0
        finally:
            await stop(worker)

    asyncio.run(go())


@pytest.mark.parametrize("state", [
    {"can_eyes": True, "talking": True, "closed": False},      # the owner is holding the button
    {"can_eyes": True, "talking": False, "closed": True},      # the socket is gone
    {"can_eyes": False, "talking": False, "closed": False},    # the app cannot draw them
])
def test_no_target_while_talking_closed_or_unable(state):
    async def go():
        calls = phone_call.PhoneCalls(lambda d: None, None, None)
        ws = SimpleNamespace(closed=state["closed"], sent=[])
        calls.active = SimpleNamespace(can_eyes=state["can_eyes"], talking=state["talking"], ws=ws)
        assert calls.eyes_call() is None and calls.eyes_live() is False
        assert await calls.send_expression({"cmd": "expression", "id": 9, "label": "sad", "ttl_ms": 4000}) is False

    asyncio.run(go())


def test_a_malformed_command_is_not_sent():
    async def go():
        calls = phone_call.PhoneCalls(lambda d: None, None, None)
        sent: list[dict] = []

        async def send_expression(msg):
            sent.append(msg)
            return True

        calls.active = SimpleNamespace(can_eyes=True, talking=False, ws=SimpleNamespace(closed=False),
                                       send_expression=send_expression)
        assert await calls.send_expression({"cmd": "expression", "label": "sad", "ttl_ms": 4000}) is False
        assert await calls.send_expression({"cmd": "expression", "id": 3, "ttl_ms": 4000}) is False
        assert await calls.send_expression(
            {"cmd": "expression", "id": 3, "label": "sad", "ttl_ms": 4000, "chirp": False}) is True
        assert sent == [{"type": "expression", "id": 3, "label": "sad", "ttl_ms": 4000}]

    asyncio.run(go())


# ---- a failed or late pick sends nothing ------------------------------------------------------------------------

def test_a_model_fault_sends_nothing(tmp_path):
    class Broken(Model):
        def predict(self, state):
            raise TimeoutError("jev timed out")

    async def go():
        server, url = await serve(tmp_path)
        service = picker(server.calls, model_factory=Broken)
        worker = asyncio.create_task(service.run())
        try:
            async with connect(url) as ws:
                await hello(ws)
                await press(ws)
                got = await until(ws, is_("state", state="listening"))
                await eventually(lambda: service.dropped >= 1 and "jev timed out" in service.error)
                await asyncio.sleep(0.1)
                assert expressions(got) == [] and service.sent == 0
        finally:
            await stop(worker)
            await server.close()

    asyncio.run(go())


def test_a_late_pick_sends_nothing():
    """The model answered after the 4 s window: stale, dropped, never sent."""
    async def go():
        now = [100.0]

        class Late(Model):
            def predict(self, state):
                now[0] += 5
                return super().predict(state)

        sent: list[dict] = []

        async def send(cmd):
            sent.append(cmd)
            return True

        service = LiveExpressions(send, enabled=True, lazy=True, model_factory=Model, clock=lambda: now[0])
        service.model, service.ready = Late(), True
        worker = asyncio.create_task(service.run())
        try:
            service.offer("assistant", "That is wonderful.")
            await eventually(lambda: service.dropped == 1)
            assert sent == []
        finally:
            await stop(worker)

    asyncio.run(go())


def test_a_lazy_picker_loads_only_at_the_first_offer_and_a_failed_load_drops_it():
    async def go():
        loads = [0]

        def factory():
            loads[0] += 1
            raise RuntimeError("no Jev key")

        sent: list[dict] = []

        async def send(cmd):
            sent.append(cmd)
            return True

        service = LiveExpressions(send, enabled=True, lazy=True, model_factory=factory)
        worker = asyncio.create_task(service.run())
        try:
            await asyncio.sleep(0.1)
            assert loads[0] == 0                         # no call from the phone, no model call
            service.offer("user", "Hello there.")
            await eventually(lambda: loads[0] == 1 and service.dropped == 1)
            assert service.pending is None and sent == [] and "no Jev key" in service.error
        finally:
            await stop(worker)

    asyncio.run(go())


# ---- the voice loop never waits on the picker -------------------------------------------------------------------

def test_a_slow_pick_does_not_hold_up_the_reply(tmp_path):
    started, release = threading.Event(), threading.Event()

    class Slow(Model):
        def predict(self, state):
            started.set()
            release.wait(5)
            return super().predict(state)

    async def go():
        server, url = await serve(tmp_path)
        service = picker(server.calls, model_factory=Slow)
        worker = asyncio.create_task(service.run())
        try:
            async with connect(url) as ws:
                await hello(ws)
                await press(ws)
                # the whole reply is heard, captioned and spoken, while the model is still thinking
                got = await until(ws, is_("state", state="listening"))
                assert started.is_set() and not release.is_set()
                assert any(isinstance(m, bytes) for m in got)
                assert [m for m in got if isinstance(m, dict) and m.get("type") == "said"]
        finally:
            release.set()
            await stop(worker)
            await server.close()

    asyncio.run(go())


# ---- the switch and the daemon's wiring -------------------------------------------------------------------------

@pytest.mark.parametrize("value,on", [(None, True), ("", True), ("1", True), ("0", False), ("off", False),
                                      ("False", False), ("no", False)])
def test_the_phone_switch(value, on):
    assert phone_expressions_on({} if value is None else {"EVE_PHONE_EXPRESSIONS": value}) is on


def test_the_daemon_wires_the_phone_picker_on_by_default(tmp_path, monkeypatch):
    board_file = tmp_path / "expressions.json"
    board_file.write_text(json.dumps({"enabled": False}))   # the board's switch, off: the phone does not need it
    monkeypatch.setenv("EVE_EXPRESSIONS_FILE", str(board_file))
    monkeypatch.delenv("EVE_PHONE_EXPRESSIONS", raising=False)

    async def go():
        calls = phone_call.PhoneCalls(lambda d: None, None, None)
        me = SimpleNamespace(_phone_expressions=None)
        tasks: list[asyncio.Task] = []
        Daemon._start_phone_expressions(me, calls, tasks)
        try:
            service = me._phone_expressions
            assert service is not None and service.enabled and service.lazy and service.path is None
            assert calls.on_text == service.offer and len(tasks) == 1
            assert service.connected() is False                  # no call yet
        finally:
            for t in tasks:
                await stop(t)

    asyncio.run(go())
    assert json.loads(board_file.read_text()) == {"enabled": False}


def test_the_daemon_leaves_the_phone_picker_off_when_switched_off(monkeypatch):
    monkeypatch.setenv("EVE_PHONE_EXPRESSIONS", "0")

    async def go():
        calls = phone_call.PhoneCalls(lambda d: None, None, None)
        me = SimpleNamespace(_phone_expressions=None)
        tasks: list[asyncio.Task] = []
        Daemon._start_phone_expressions(me, calls, tasks)
        assert me._phone_expressions is None and calls.on_text is None and tasks == []

    asyncio.run(go())


def test_react_goes_to_the_phone_when_no_board_is_connected(tmp_path):
    async def go():
        board = LiveExpressions(lambda cmd: None, path=tmp_path / "board.json", model_factory=Model)   # off
        live = [True]
        phone = LiveExpressions(lambda cmd: None, connected=lambda: live[0], enabled=True, lazy=True,
                                model_factory=Model)
        me = SimpleNamespace(_expressions=board, _phone_expressions=phone, ble=SimpleNamespace(connected=False))
        got = await Daemon._ipc_expressions(me, {"action": "react", "text": "That is wonderful news!"})
        assert got["ok"] and got["id"] == phone.pending[0] and board.pending is None
        status = await Daemon._ipc_expressions(me, {"action": "status"})
        assert status["phone"]["connected"] is True and status["phone"]["enabled"] is True
        live[0] = False                                   # no call: the old refusal
        refused = await Daemon._ipc_expressions(me, {"action": "react", "text": "Another one."})
        assert refused["ok"] is False and "disconnected" in refused["error"]

    asyncio.run(go())
