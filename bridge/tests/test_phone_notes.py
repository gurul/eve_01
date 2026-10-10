"""phone_call.PhoneNotes: "start taking notes" said on an Eve Link call records the phone until the Action Button.

The owner asked on 2026-10-04: "when i say start taking notes the phone should hold the port until i click action
button to turn it off, this allows me to take notes on the go". The real Mini App server on a loopback port, the
Eve Link door (/api/stick, a token instead of initData), a real WebSocket client as the phone, a chat that runs
take_notes the way telegram.py does, and a fake transcriber (no audio leaves the machine, no model is called)."""
from __future__ import annotations

import asyncio
import contextlib
import json
import struct
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from test_notes import SUMMARY, FakeTranscriber
from test_phone_call import OWNER, TOKEN, Voice, is_, signed, until
from websockets.asyncio.client import connect
from websockets.exceptions import ConnectionClosed

from eve_bridge import phone_call
from eve_bridge.miniapp import MiniAppConfig, MiniAppServer, SpendLedger, check_init_data
from eve_bridge.notes import NotesConfig, RoomNotes
from eve_bridge.recall import RecallConfig

LINK = "link-token"
LOUD = struct.pack("<2400h", *([6000, -6000] * 1200))        # 100 ms at 24 kHz, well above SILENCE_RMS


class NotesBrain:
    """The chat: "start taking notes" runs take_notes with the call's reader, as telegram._robot_tool does, and
    the reply is read out through the reader the turn began with."""

    def __init__(self) -> None:
        self.heard: list[str] = []
        self.say: Any = None
        self.calls: Any = None
        self.results: list[Any] = []

    def listen(self, say, owner=None) -> bool:
        if say is not None or owner is None or self.say == owner:
            self.say = say
        return True

    def chat_ready(self) -> bool:
        return True

    def hear(self, text: str) -> None:
        self.heard.append(text)
        say = self.say                                              # fixed for the turn, as telegram.py's call_say

        async def turn() -> None:
            action = "stop" if "stop" in text else "start"
            result = await self.calls.notes_action(action, say)
            self.results.append(result)
            if say is not None:
                say("Taking notes." if result and result.get("ok") else "I can't.")

        asyncio.ensure_future(turn())


async def serve(tmp_path: Path, brain: NotesBrain, client: FakeTranscriber | None = None, notes: bool = True):
    cfg = MiniAppConfig(enabled=True, token=TOKEN, owner_ids=frozenset({OWNER}), ledger_path=tmp_path / "l.json")
    server = MiniAppServer(cfg, SpendLedger(cfg.ledger_path), page=b"home")
    voice = Voice(text="start taking notes")
    server.calls = phone_call.PhoneCalls(lambda d: check_init_data(d, TOKEN, cfg.owner_ids), brain, voice)
    server.stick = SimpleNamespace(check=lambda token: token == LINK)
    told: list[tuple[str, str]] = []

    async def notify(text: str, about: str = "") -> None:
        told.append((text, about))

    recall = RecallConfig(store=tmp_path / "debrief", notes=tmp_path / "notes")
    transcriber = client or FakeTranscriber(["first we talk about the servo", "then the case gets reprinted"])
    if notes:
        server.calls.notes = phone_call.PhoneNotes(
            lambda mic: RoomNotes(recall, NotesConfig(segment_secs=0.5), transcriber, mic, source="eve-link"),
            notify)
    brain.calls = server.calls
    port = await server.start()
    return SimpleNamespace(server=server, voice=voice, told=told, recall=recall, transcriber=transcriber,
                           stick=f"ws://127.0.0.1:{port}/api/stick", call=f"ws://127.0.0.1:{port}/api/call",
                           origin=f"http://127.0.0.1:{port}")


async def hello(ws, can=("notes",)) -> None:
    await ws.send(json.dumps({"token": LINK, "can": list(can)}))
    await until(ws, is_("state", state="connected"))


async def say_start(ws) -> list[Any]:
    """The owner presses and says "start taking notes"; eve answers and the phone is told notes are on."""
    await ws.send(json.dumps({"type": "talk"}))
    for _ in range(10):
        await ws.send(bytes(4800))
    await ws.send(json.dumps({"type": "done"}))
    return await until(ws, is_("state", state="notes"))


async def talk_for(ws, secs: float) -> None:
    for _ in range(int(secs * 10)):
        await ws.send(LOUD)
        await asyncio.sleep(0.01)


async def eventually(pred, secs: float = 5.0) -> None:
    async with asyncio.timeout(secs):
        while not pred():
            await asyncio.sleep(0.02)


def files(recall: RecallConfig) -> list[Path]:
    return sorted((recall.transcripts_dir / "meetings").rglob("*.md"))


def test_start_on_the_phone_records_the_phone_and_the_action_button_writes_them_up(tmp_path):
    brain = NotesBrain()

    async def go():
        h = await serve(tmp_path, brain)
        async with connect(h.stick) as ws:
            await hello(ws)
            notes_on = (await say_start(ws))[-1]
            assert notes_on["started"] > 0 and notes_on["words"] == 0
            assert h.server.calls.holding_chat() is False and brain.say is None   # texts are not read into it
            # eve's spoken confirmation, then back to notes (not "listening": the phone keeps recording)
            after = await until(ws, is_("state", state="notes"))
            assert any(isinstance(m, dict) and m.get("state") == "speaking" for m in after)
            await talk_for(ws, 1.2)                                       # audio with no talk: the notes', not a press
            await eventually(lambda: len(h.server.calls.notes.taker.lines) >= 2)
            await ws.send(json.dumps({"type": "notes_stop"}))
            got = await until(ws, is_("state", state="listening"))
            await until(ws, lambda m: isinstance(m, bytes))              # "Notes saved." spoken on the call
            await eventually(lambda: bool(h.told))
        await h.server.close()
        return h, got

    h, got = asyncio.run(go())
    assert {"type": "state", "state": "thinking", "note": "Writing up your notes."} in got
    assert got[-1] == {"type": "state", "state": "listening", "note": f"Notes saved: {SUMMARY['title']}"}
    assert brain.heard == ["start taking notes"]                       # one turn: the notes were never presses
    assert brain.results[0]["ok"] and brain.results[0]["on"] == "your phone"
    assert "Notes saved." in h.voice.spoken
    [path] = files(h.recall)
    text = path.read_text()
    assert "source: eve-link" in text and "first we talk about the servo" in text
    assert "stopped_by: you pressed stop on the phone" in text
    message, about = h.told[0]
    assert SUMMARY["title"] in message and SUMMARY["actions"][0]["what"] in message and path.name in message
    assert path.name in about and "talk about" not in about                # the history never gets the words heard


def test_an_old_app_is_refused_and_the_mini_app_leaves_it_to_the_room(tmp_path):
    brain = NotesBrain()

    async def go():
        h = await serve(tmp_path, brain)
        async with connect(h.stick) as ws:
            await hello(ws, can=())                                     # an Eve Link build from before notes
            await ws.send(json.dumps({"type": "talk"}))
            for _ in range(10):
                await ws.send(bytes(4800))
            await ws.send(json.dumps({"type": "done"}))
            await eventually(lambda: bool(brain.results))
        assert brain.results[-1]["ok"] is False and "update" in brain.results[-1]["reason"]
        async with connect(h.call, origin=h.origin) as ws:
            await ws.send(json.dumps({"initData": signed()}))
            await until(ws, is_("state", state="connected"))
            await ws.send(json.dumps({"type": "talk"}))
            for _ in range(10):
                await ws.send(bytes(4800))
            await ws.send(json.dumps({"type": "done"}))
            await eventually(lambda: len(brain.results) == 2)
        assert brain.results[-1] is None                                # the room's note-taker answers
        assert not h.server.calls.notes.active
        # and nothing said on a call at all (a text, the desk) is never the phone's start
        assert await h.server.calls.notes_action("start", None) is None
        await h.server.close()

    asyncio.run(go())


def test_a_dropped_call_keeps_what_was_heard_and_the_phone_calling_back_carries_on(tmp_path):
    brain = NotesBrain()

    async def go():
        h = await serve(tmp_path, brain)
        calls = h.server.calls
        async with connect(h.stick) as ws:
            await hello(ws)
            first = (await say_start(ws))[-1]
            await talk_for(ws, 0.6)
            await eventually(lambda: len(calls.notes.taker.lines) >= 1)
        await eventually(lambda: calls.active is None)
        assert calls.notes.recording and calls.notes.grace is not None    # waiting for the phone
        async with connect(h.stick) as ws:
            await ws.send(json.dumps({"token": LINK, "can": ["notes"]}))
            back = (await until(ws, is_("state", state="notes")))[-1]
            assert back["started"] == first["started"] and back["words"] > 0
            assert calls.notes.grace is None
            await talk_for(ws, 0.6)
            await eventually(lambda: len(calls.notes.taker.lines) >= 2)
            await ws.send(json.dumps({"type": "talk"}))                 # a build without notes_stop: talk stops
            await until(ws, is_("state", state="listening"))
        await eventually(lambda: bool(h.told))
        await h.server.close()
        return h

    h = asyncio.run(go())
    [path] = files(h.recall)
    text = path.read_text()
    assert "first we talk about the servo" in text and "then the case gets reprinted" in text


def test_a_phone_that_never_calls_back_gets_its_notes_after_the_grace(tmp_path, monkeypatch):
    monkeypatch.setattr(phone_call, "NOTES_GRACE_SECS", 0.3)
    brain = NotesBrain()

    async def go():
        h = await serve(tmp_path, brain)
        async with connect(h.stick) as ws:
            await hello(ws)
            await say_start(ws)
            await talk_for(ws, 0.6)
            await eventually(lambda: len(h.server.calls.notes.taker.lines) >= 1)
        await eventually(lambda: bool(h.told))
        assert not h.server.calls.notes.active
        await h.server.close()
        return h

    h = asyncio.run(go())
    [path] = files(h.recall)
    assert "stopped_by: the phone dropped" in path.read_text()


def test_the_end_button_stops_and_writes_them_up_at_once(tmp_path, monkeypatch):
    monkeypatch.setattr(phone_call, "NOTES_GRACE_SECS", 60.0)
    brain = NotesBrain()

    async def go():
        h = await serve(tmp_path, brain)
        async with connect(h.stick) as ws:
            await hello(ws)
            await say_start(ws)
            await talk_for(ws, 0.6)
            await eventually(lambda: len(h.server.calls.notes.taker.lines) >= 1)
            await ws.send(json.dumps({"type": "end"}))
            assert (await until(ws, is_("ended")))[-1]["reason"] == phone_call.HUNG_UP
        await eventually(lambda: bool(h.told), secs=3)
        await h.server.close()
        return h

    h = asyncio.run(go())
    assert "stopped_by: you ended the call" in files(h.recall)[0].read_text()


def test_notes_outlast_the_calls_hour_and_the_command_stops_them(tmp_path, monkeypatch):
    brain = NotesBrain()

    async def go():
        h = await serve(tmp_path, brain)
        calls = h.server.calls
        async with connect(h.stick) as ws:
            await hello(ws)
            await say_start(ws)
            monkeypatch.setattr(phone_call, "MAX_CALL_SECS", 0.3)      # the call's "hour", from here
            await talk_for(ws, 1.0)                                      # past the call's cap: still recording
            assert calls.active is not None and calls.notes.recording
            assert calls.holding_chat() is False                        # the desk's button is free meanwhile
            status = await calls.notes_action("status")
            assert status["on"] == "your phone" and status["attached"] and status["active"]
            out = await calls.notes_action("stop", reason="asked")      # `eve-bridge notes stop`
            assert out["ok"] and out["path"] and out["texted"]
            await until(ws, is_("state", state="listening"))
        assert await calls.notes_action("stop") is None                # nothing on: the room's taker answers
        await h.server.close()

    asyncio.run(go())


def test_pushed_audio_feeds_only_a_listening_session():
    from eve_bridge.notes import PushedAudio

    mic = PushedAudio()

    async def go():
        assert mic.put(LOUD) is False                                   # nobody subscribed: dropped
        q = mic.subscribe()
        assert mic.put(LOUD + b"\x01") is True and len(q.get_nowait()) == len(LOUD)   # whole samples only
        mic.unsubscribe(q)
        assert mic.put(LOUD) is False

    asyncio.run(go())


def test_nothing_said_is_said_and_texted_plainly(tmp_path):
    brain = NotesBrain()

    async def go():
        h = await serve(tmp_path, brain)
        async with connect(h.stick) as ws:
            await hello(ws)
            await say_start(ws)
            await ws.send(json.dumps({"type": "notes_stop"}))
            got = await until(ws, is_("state", state="listening"))
        await eventually(lambda: bool(h.told))
        await h.server.close()
        return h, got

    h, got = asyncio.run(go())
    assert got[-1]["note"] == "Nothing was said, so there are no notes."
    assert "nothing was said" in h.told[0][0] and files(h.recall) == []


def test_the_notes_command_reaches_the_phones_notes_and_starts_only_the_rooms():
    """`eve-bridge notes stop|status` stops the phone's notes while they are on; `notes start` is the desk's."""
    from eve_bridge.daemon import Daemon

    asked: list = []
    room: list = []

    class Calls:
        notes = object()

        async def notes_action(self, action, call_say=None, reason="asked"):
            asked.append((action, reason))
            return {"ok": True, "on": "your phone"} if action == "stop" else None

    class Room:
        def start(self):
            room.append("start")
            return {"ok": True, "started": True}

        def status(self):
            return {"active": False}

    me = SimpleNamespace(_miniapp=SimpleNamespace(server=SimpleNamespace(calls=Calls())),
                         _room_notes_taker=lambda: Room())

    async def go():
        return [await Daemon._ipc_notes(me, {"action": a, "reason": "asked"}) for a in ("stop", "status", "start")]

    stop, status, start = asyncio.run(go())
    assert stop["on"] == "your phone" and status == {"ok": True, "active": False} and start["started"]
    assert asked == [("stop", "asked"), ("status", "asked")] and room == ["start"]


def test_the_daemon_lends_phone_notes_that_write_eve_link_files(tmp_path):
    from eve_bridge.daemon import Daemon

    told: list = []

    class Inlet:
        async def tell_owner(self, text, about="", title=""):
            told.append(title)

    me = SimpleNamespace(_recall_cfg=RecallConfig(store=tmp_path / "debrief", notes=tmp_path / "notes"))
    notes = Daemon._phone_notes(me, Inlet())
    taker = notes.make_taker(object())
    assert isinstance(notes, phone_call.PhoneNotes) and taker.source == "eve-link" and taker.cfg is me._recall_cfg
    asyncio.run(notes.notify("x", "y"))
    assert told == ["Notes"]


def test_a_quiet_call_during_notes_closes_without_an_end_so_the_phone_calls_back(tmp_path, monkeypatch):
    """Review, 2026-10-04: the 30 s quiet timeout used to send ``ended``, which the phone treats as an end eve
    chose (no call back). During notes it now just closes the socket: the phone sees a drop, and eve waits."""
    monkeypatch.setattr(phone_call, "NOTES_GRACE_SECS", 60.0)
    brain = NotesBrain()

    async def go():
        h = await serve(tmp_path, brain)
        calls = h.server.calls
        async with connect(h.stick) as ws:
            await hello(ws)
            await say_start(ws)
            await until(ws, is_("state", state="notes"))                # after the spoken confirmation
            await talk_for(ws, 0.6)
            await eventually(lambda: len(calls.notes.taker.lines) >= 1)
            monkeypatch.setattr(phone_call, "QUIET_SECS", 0.3)        # the phone stalls from here
            await ws.send(json.dumps({"type": "ping"}))                 # (the read now waiting began before)
            rest: list = []
            with contextlib.suppress(ConnectionClosed):
                async with asyncio.timeout(5):
                    while True:
                        m = await ws.recv()
                        rest.append(m if isinstance(m, bytes) else json.loads(m))
        await eventually(lambda: calls.active is None)
        assert not any(isinstance(m, dict) and m.get("type") == "ended" for m in rest)
        assert calls.notes.recording and calls.notes.grace is not None    # waiting for the phone, not written up
        out = await calls.notes_action("stop", reason="asked")
        assert out["ok"] and out["path"]
        await h.server.close()

    asyncio.run(go())


def test_a_ping_during_notes_gets_a_pong_and_an_ordinary_call_stays_as_it_was(tmp_path):
    """The phone cannot tell a half-open connection from a quiet one while notes record (eve sends nothing):
    each ping then gets a pong. Outside notes a ping is answered by nothing, as before."""
    brain = NotesBrain()

    async def go():
        h = await serve(tmp_path, brain)
        async with connect(h.stick) as ws:
            await hello(ws)
            await ws.send(json.dumps({"type": "ping"}))
            with contextlib.suppress(TimeoutError):
                async with asyncio.timeout(0.3):
                    before = await ws.recv()
                    raise AssertionError(f"an ordinary call answered a ping: {before!r}")
            await say_start(ws)
            await until(ws, is_("state", state="notes"))
            await ws.send(json.dumps({"type": "ping"}))
            await until(ws, is_("pong"))
            await ws.send(json.dumps({"type": "notes_stop"}))
            await until(ws, is_("state", state="listening"))
        await h.server.close()

    asyncio.run(go())


def test_notes_not_written_are_never_called_saved_and_the_text_carries_the_words(tmp_path, monkeypatch):
    from eve_bridge import notes as notes_mod

    monkeypatch.setattr(notes_mod, "write_notes", lambda *a, **k: None)    # a full disk
    brain = NotesBrain()

    async def go():
        h = await serve(tmp_path, brain)
        async with connect(h.stick) as ws:
            await hello(ws)
            await say_start(ws)
            await talk_for(ws, 0.6)
            await eventually(lambda: len(h.server.calls.notes.taker.lines) >= 1)
            await ws.send(json.dumps({"type": "notes_stop"}))
            got = await until(ws, is_("state", state="listening"))
        await eventually(lambda: bool(h.told))
        await h.server.close()
        return h, got

    h, got = asyncio.run(go())
    assert "saved:" not in got[-1]["note"].lower() and "texted" in got[-1]["note"]
    assert "Notes saved." not in h.voice.spoken
    message = h.told[0][0]
    assert "only copy" in message and "first we talk about the servo" in message and "is saved" not in message
    assert "talk about" not in h.told[0][1]                                 # the history still never gets them


def test_the_daemon_stopping_writes_the_phones_notes_at_once(tmp_path):
    """Notes live in memory until written: a daemon stopping mid-notes writes them now, transcript only."""
    from eve_bridge.daemon import Daemon

    brain = NotesBrain()

    class NoSummary(FakeTranscriber):
        def summarize(self, transcript: str) -> str:
            raise AssertionError("a shutdown must not wait for a summary")

    async def go():
        h = await serve(tmp_path, brain, client=NoSummary(["first we talk about the servo"]))
        async with connect(h.stick) as ws:
            await hello(ws)
            await say_start(ws)
            await talk_for(ws, 0.6)
            await eventually(lambda: len(h.server.calls.notes.taker.lines) >= 1)
            me = SimpleNamespace(_miniapp=SimpleNamespace(server=h.server), _room_notes=None)
            await Daemon._close_notes(me)
        assert not h.server.calls.notes.active
        await h.server.close()
        return h

    h = asyncio.run(go())
    [path] = files(h.recall)
    text = path.read_text()
    assert "stopped_by: the daemon stopped" in text and "first we talk about the servo" in text
    assert h.told                                                            # and texted, as any stop is


def test_the_daemon_stopping_writes_the_rooms_notes_too(tmp_path):
    from test_notes import FakeEars

    from eve_bridge.daemon import Daemon

    class NoSummary(FakeTranscriber):
        def summarize(self, transcript: str) -> str:
            raise AssertionError("a shutdown must not wait for a summary")

    recall = RecallConfig(store=tmp_path / "debrief", notes=tmp_path / "notes")

    async def go():
        room = RoomNotes(recall, NotesConfig(segment_secs=0.5), NoSummary(), FakeEars())
        assert room.start()["ok"]
        await eventually(lambda: len(room.lines) >= 1)
        await Daemon._close_notes(SimpleNamespace(_room_notes=room))
        return room

    room = asyncio.run(go())
    assert not room.active and room.state.path is not None
    assert "stopped_by: the daemon stopped" in room.state.path.read_text()


def test_notes_drop_a_press_they_cut_off_on_both_sides():
    """A press still open when notes start (said while eve was answering) is nobody's question now: the phone
    drops its turn (Relay.notesOn) and the call forgets it too, so the first press after the notes is a real one."""

    class Ws:
        closed = True

        async def send_json(self, obj) -> None:
            pass

    brain = NotesBrain()
    call = phone_call.Call(Ws(), brain, Voice(text="x"))
    call.talking, call.press = True, bytearray(b"\x01\x02")
    notes = phone_call.PhoneNotes(lambda mic: None)

    async def go():
        await notes.attach(call)
        assert call.talking is False and call.press == bytearray() and call.notes is notes
        # positive control: with notes off a talk begins a press, which a stuck ``talking`` used to swallow
        call.notes = None
        await call._begin_press()
        assert call.talking is True
        if call.reading is not None:
            call.reading.cancel()
            await asyncio.gather(call.reading, return_exceptions=True)

    asyncio.run(go())


def test_the_phones_status_never_carries_the_words_heard():
    notes = phone_call.PhoneNotes(lambda mic: None)
    notes.taker = SimpleNamespace(status=lambda: {"active": True, "words": 9, "last": "the secret plan"})
    out = notes.status()
    assert "last" not in out and out["words"] == 9
    assert "the secret plan" not in json.dumps(out)
