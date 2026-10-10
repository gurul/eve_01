"""The daemon side of the manual explore: `eve-bridge explore` over IPC, a
touch on the board, the wake word, and "hey eve, go explore" (the voice tool
asks; the explore starts once the conversation has closed). No board, no mic,
no network: the same SimpleNamespace + MethodType stub test_explore.py uses."""

from __future__ import annotations

import asyncio
import base64 as _b64
import logging
from pathlib import Path
from types import MethodType, SimpleNamespace

from eve_bridge import daemon as daemon_mod
from eve_bridge.caption_pager import CaptionPager
from eve_bridge.daemon import Daemon
from eve_bridge.daemon import Daemon as _Daemon
from eve_bridge.diary import Thought
from eve_bridge.explore import WAYPOINTS, ExploreConfig, Explorer
from eve_bridge.recall import RecallConfig
from eve_bridge.scene import SceneWatcher
from eve_bridge.thought_screen import ThoughtScreen

MODE_ON = {"cmd": "mode", "explore": True}


def _first_look() -> dict:
    """The look the explorer opens a pan with, whatever the pan plan is."""
    yaw, pitch = WAYPOINTS[0]
    return {"cmd": "look", "yaw": yaw, "pitch": pitch, "hold": 6000}

MODE_OFF = {"cmd": "mode", "explore": False}


class _Ble:
    def __init__(self, connected: bool = True) -> None:
        self.connected = connected
        self.sent: list[dict] = []

    async def send(self, obj: dict, codec=None) -> bool:
        self.sent.append(obj)
        return True


class _Conversation:
    def __init__(self) -> None:
        self.cancelled = False

    def done(self) -> bool:
        return self.cancelled

    def cancel(self) -> None:
        self.cancelled = True


def _cfg(enabled: bool = True) -> ExploreConfig:
    return ExploreConfig(enabled=enabled, after_secs=600.0, notes_per_hour=0.0, model="fake",
                         notes_dir=Path("/nonexistent"), cycle_wait_secs=900.0)


def _daemon(connected: bool = True, listen_sent=None, enabled: bool = True) -> SimpleNamespace:
    d = SimpleNamespace(
        ble=_Ble(connected),
        state=SimpleNamespace(running_count=0, waiting_count=0),
        _listen_sent=listen_sent,
        _listen_down=False,
        _explore_cfg=_cfg(enabled),
        _explorer=Explorer(_cfg(enabled), now=0.0, notes_enabled=False),
        _notes=None,
        _last_activity_at=0.0,
        _explore_raw_frame=None,
        _conversation=None,
        _active_agent=None,
        _agent_state="idle",
        _explore_after_conversation=None,
        _voice=SimpleNamespace(start=lambda: None, stop=lambda: None, tap=lambda name: None),
        _voice_cfg=None,
        # recall.py / memory.py: eve's memory of talking with the owner. Memory off (None) here: an
        # empty brief and nothing written, which is what these tests want.
        _recall_cfg=RecallConfig(store=Path("/nonexistent/memory"), notes=Path("/nonexistent/notes")),
        _memory=None,
        _voice_conv="",
        _background=set(),
        _agent_cfg=SimpleNamespace(enabled=False),
        _last_diag=None,
        # The voice's eyes, head and mute (scene.py / head.py / sound.py): inert here.
        _scene=SceneWatcher(None),
        _head=SimpleNamespace(observe=lambda yaw, pitch: None),
        _sound=SimpleNamespace(on=True, muted=False),
        _intent=None,
        _thinker=None,
        _photo_for_owner=None,
        _set_sound=lambda on: None,
    )
    d._thought_pager = CaptionPager()
    d._screen = ThoughtScreen()
    for name in ("_note_activity", "_idle_secs", "_explore_step", "_run_explore_action", "_stop_explore",
                 "_request_explore", "_dismiss_explore", "_clear_thought", "_flush_thought_pager",
                 "_show_thought", "_handle_ipc", "_handle_ble", "_on_wake",
                 "_converse", "_on_voice_explore", "_on_agent_state", "_resync_agent", "_agent_keepalive",
                 "_cancel_active_task", "_make_agent", "_wake_suppressed", "_on_caption"):
        setattr(d, name, MethodType(getattr(Daemon, name), d))
    return d


# ---- IPC: eve-bridge explore [start|stop|status] ---------------------------------------

def test_ipc_start_puts_the_board_in_explore_mode_without_counting_as_activity() -> None:
    async def go():
        d = _daemon()
        d._last_activity_at = 123.0
        resp = await d._handle_ipc({"evt": "explore", "action": "start"})
        assert resp["ok"] and resp["connected"]
        assert resp["explore"]["state"] == "exploring" and resp["explore"]["manual"] is True
        assert resp["explore"]["reason"] == "requested by cli"
        assert d.ble.sent == [MODE_ON, _first_look()]
        assert d._last_activity_at == 123.0            # the request itself is not "the human is back"
        # a tick with zero idle time (a session is running) keeps exploring
        d.state.running_count = 1
        await d._explore_step(7.0)
        assert d._explorer.active
        assert d.ble.sent[-1]["cmd"] == "look"
    asyncio.run(go())


def test_ipc_stop_ends_it_and_status_reports() -> None:
    async def go():
        d = _daemon()
        st = await d._handle_ipc({"evt": "explore", "action": "status"})
        assert st == {"ok": True, "connected": True, "explore": d._explorer.status()}
        assert st["explore"]["state"] == "off"
        await d._handle_ipc({"evt": "explore", "action": "start"})
        st = await d._handle_ipc({"evt": "explore", "action": "status"})
        assert st["explore"]["state"] == "exploring" and st["explore"]["waypoint"] == list(WAYPOINTS[0])
        resp = await d._handle_ipc({"evt": "explore", "action": "stop"})
        assert resp["ok"] and resp["explore"]["state"] == "off" and resp["explore"]["manual"] is False
        assert d.ble.sent[-1] == MODE_OFF
        assert d.ble.sent.count(MODE_OFF) == 1
        # stop when nothing is running is a harmless no-op on the wire
        n = len(d.ble.sent)
        resp = await d._handle_ipc({"evt": "explore", "action": "stop"})
        assert resp["ok"] and len(d.ble.sent) == n
    asyncio.run(go())


def test_ipc_start_is_refused_while_disconnected_or_dictating() -> None:
    async def go():
        for d, why in ((_daemon(connected=False), "board disconnected"),
                       (_daemon(listen_sent=True), "listen key")):
            resp = await d._handle_ipc({"evt": "explore", "action": "start"})
            assert resp["ok"] is False and resp["error"] == why, why
            assert resp["explore"]["state"] == "off"
            assert d.ble.sent == []
        resp = await _daemon()._handle_ipc({"evt": "explore", "action": "dance"})
        assert resp["ok"] is False and "unknown explore action" in resp["error"]
    asyncio.run(go())


def test_ipc_start_works_when_the_idle_start_is_disabled() -> None:
    async def go():
        d = _daemon(enabled=False)
        await d._explore_step(5000.0)                   # never starts on its own
        assert d.ble.sent == []
        resp = await d._handle_ipc({"evt": "explore"})  # action defaults to start
        assert resp["ok"] and d.ble.sent[0] == MODE_ON
    asyncio.run(go())


# ---- the board and the mic --------------------------------------------------------------------

def test_a_touch_ends_a_manual_explore_at_once() -> None:
    async def go():
        d = _daemon()
        d._keys = SimpleNamespace(tap=lambda name: None)
        await d._handle_ipc({"evt": "explore", "action": "start"})
        await d._handle_ble({"cmd": "key", "name": "enter"})
        assert d._explorer.state == "off" and not d._explorer.manual
        assert d.ble.sent[-1] == MODE_OFF
        # the idle clock restarted: the next tick does not resume
        await d._explore_step(1.0)
        assert d._explorer.state == "off"
    asyncio.run(go())


def test_a_touch_during_a_conversation_ends_the_explore_but_not_the_conversation() -> None:
    async def go():
        d = _daemon()
        conv = _Conversation()
        d._conversation = conv
        d._keys = SimpleNamespace(tap=lambda name: None)
        await d._handle_ipc({"evt": "explore", "action": "start"})
        await d._handle_ble({"cmd": "key", "name": "enter"})
        assert not conv.cancelled and d._explorer.state == "off"
        assert d.ble.sent[-1] == MODE_OFF
    asyncio.run(go())


def test_the_wake_word_ends_a_manual_explore_before_the_conversation(monkeypatch) -> None:
    async def go():
        d = _daemon()
        d._ears = None                                   # _converse returns at once without ears
        await d._handle_ipc({"evt": "explore", "action": "start"})
        d._on_wake("hey eve")
        await asyncio.sleep(0)                           # let the dismiss task run
        await asyncio.sleep(0)
        assert d._explorer.state == "off" and not d._explorer.manual
        assert d.ble.sent[-1] == MODE_OFF
        await d._conversation
    asyncio.run(go())


def test_the_lesson_word_opens_a_conversation_flagged_as_a_lesson_wake(monkeypatch) -> None:
    calls: list[dict] = []

    async def fake_converse(self, think_aloud=None, lesson_wake=False):
        calls.append({"lesson_wake": lesson_wake})

    monkeypatch.setattr(daemon_mod.Daemon, "_converse", fake_converse)

    async def go():
        d = _daemon()
        d._ears_cfg = daemon_mod.ears_configured({})
        d._on_wake("lesson")
        await d._conversation
        d._on_wake("hey_eve")
        await d._conversation
        assert calls == [{"lesson_wake": True}, {"lesson_wake": False}]
    asyncio.run(go())


def test_voice_go_explore_starts_once_the_conversation_has_closed(monkeypatch) -> None:
    order: list[str] = []

    async def fake_open_session(mic, on_state, agent_factory, **kw):
        on_state("listening")
        kw["on_explore"]()
        order.append("session closed")
        assert not any(m.get("cmd") == "mode" for m in mics_daemon.ble.sent)   # not yet

    monkeypatch.setattr(daemon_mod.voice_agent, "open_session", fake_open_session)

    class _Ears:
        def subscribe(self):
            return asyncio.Queue()

        def unsubscribe(self, q):
            order.append("unsubscribed")

    async def go():
        global mics_daemon
        mics_daemon = d = _daemon()
        d._ears = _Ears()
        await d._converse()
        await asyncio.sleep(0)                           # the agent-state send is a task of its own
        assert order == ["session closed", "unsubscribed"]
        assert d._explorer.active and d._explorer.manual
        assert d._explorer.started_reason == "requested by voice"
        assert d._explore_after_conversation is None
        assert MODE_ON in d.ble.sent
        assert _first_look() in d.ble.sent
        assert {"cmd": "agent", "state": "idle"} in d.ble.sent
        assert d.ble.sent.index(MODE_ON) > d.ble.sent.index({"cmd": "agent", "state": "listening"})
    asyncio.run(go())


def test_voice_go_explore_is_dropped_when_the_conversation_is_hushed(monkeypatch) -> None:
    async def fake_open_session(mic, on_state, agent_factory, **kw):
        kw["on_explore"]()
        raise asyncio.CancelledError()

    monkeypatch.setattr(daemon_mod.voice_agent, "open_session", fake_open_session)

    class _Ears:
        def subscribe(self):
            return asyncio.Queue()

        def unsubscribe(self, q):
            pass

    async def go():
        d = _daemon()
        d._ears = _Ears()
        try:
            await d._converse()
        except asyncio.CancelledError:
            pass
        assert d._explorer.state == "off" and d._explore_after_conversation is None
        assert not any(m == MODE_ON for m in d.ble.sent)
    asyncio.run(go())


# ---- photos: {"cmd":"snap"} and the one frame line that answers it ----------------------------


_JPEG = b"\xff\xd8\xff\xe0" + b"\x00" * 16 + b"\xff\xd9"


def _snap_daemon(connected: bool = True) -> SimpleNamespace:
    d = _daemon(connected=connected)
    d._snap_waiter = None
    d._vision = SimpleNamespace(on_frame=_record_stream_frame(d))
    for name in ("_take_snapshot",):
        setattr(d, name, MethodType(getattr(_Daemon, name), d))
    d.SNAP_TIMEOUT_SECS = 0.05
    return d


def _record_stream_frame(d):
    d.stream_frames = []

    async def on_frame(obj):
        d.stream_frames.append(obj)
    return on_frame


def _snap_line(snap: bool = True, w: int = 320, h: int = 240) -> dict:
    f = {"seq": 7, "w": w, "h": h, "fmt": "jpeg", "b64": _b64.b64encode(_JPEG).decode(), "yaw": 20, "pitch": 40}
    if snap:
        f["snap"] = True
    return {"frame": f}


def test_take_snapshot_sends_snap_and_returns_the_answering_frame() -> None:
    async def go():
        d = _snap_daemon()

        async def board():
            await asyncio.sleep(0.01)
            assert d.ble.sent[-1] == {"cmd": "snap"}
            await d._handle_ble(_snap_line())

        asyncio.create_task(board())
        frame = await d._take_snapshot()
        assert frame is not None and frame.snap and (frame.w, frame.h) == (320, 240)
        assert frame.data == _JPEG and frame.yaw == 20.0
        assert d.stream_frames == []                 # never fed to the face tracker
        assert d._snap_waiter is None
    asyncio.run(go())


def test_take_snapshot_times_out_on_old_firmware_and_leaves_no_waiter(caplog) -> None:
    async def go():
        d = _snap_daemon()
        with caplog.at_level(logging.INFO):
            frame = await d._take_snapshot()
        assert frame is None and d.ble.sent == [{"cmd": "snap"}]
        assert d._snap_waiter is None
        assert any("snap not answered" in r.message for r in caplog.records)
        # a late answer after the timeout is dropped, not an error
        await d._handle_ble(_snap_line())
        assert d.stream_frames == []
    asyncio.run(go())


def test_take_snapshot_refuses_while_disconnected_or_busy() -> None:
    async def go():
        assert await _snap_daemon(connected=False)._take_snapshot() is None
        d = _snap_daemon()
        d.SNAP_TIMEOUT_SECS = 0.2
        first = asyncio.create_task(d._take_snapshot())
        await asyncio.sleep(0.01)
        assert await d._take_snapshot() is None      # one at a time
        await d._handle_ble(_snap_line())
        assert (await first) is not None
        assert d.ble.sent.count({"cmd": "snap"}) == 1
    asyncio.run(go())


def test_stream_frames_still_go_to_the_tracker_and_bad_snaps_are_none() -> None:
    async def go():
        d = _snap_daemon()
        await d._handle_ble(_snap_line(snap=False, w=160, h=120))
        assert len(d.stream_frames) == 1

        async def board():
            await asyncio.sleep(0.01)
            bad = _snap_line()
            bad["frame"]["b64"] = "not base64!"
            await d._handle_ble(bad)

        asyncio.create_task(board())
        assert await d._take_snapshot() is None
        assert d._snap_waiter is None
    asyncio.run(go())


def test_every_other_board_touch_still_ends_it() -> None:
    """focus / key: either one is the human, so either ends the explore."""
    async def go():
        for cmd in ({"cmd": "focus"}, {"cmd": "key", "name": "enter"}):
            d = _daemon()
            d._keys = SimpleNamespace(tap=lambda name: None)
            await d._handle_ipc({"evt": "explore", "action": "start"})
            try:
                await d._handle_ble(cmd)
            except AttributeError:
                # The rest of the handler needs daemon state this stub does not
                # carry; the explore must already have been dismissed by then.
                pass
            assert d._explorer.state == "off", cmd
            assert MODE_OFF in d.ble.sent, cmd
    asyncio.run(go())


# ---- thoughts on the robot's own screen while it explores ---------------------------------------

def _captions(d) -> list[dict]:
    return [m for m in d.ble.sent if m.get("cmd") == "caption"]


def _thought(text: str, tags: tuple = (), written: bool = True, photographed: bool = False,
             cool: float = 0.5, novelty: int = 8, importance: int = 7) -> Thought:
    """A thought that clears the screen's floor unless a test says otherwise."""
    return Thought(text=text, tags=tags, written=written, photographed=photographed, cool=cool,
                   novelty=novelty, importance=importance)


def _exploring_daemon() -> SimpleNamespace:
    d = _daemon()
    d._show_thought = MethodType(_Daemon._show_thought, d)
    d._flush_thought_pager = MethodType(_Daemon._flush_thought_pager, d)
    d._clear_thought = MethodType(_Daemon._clear_thought, d)
    d._on_caption = MethodType(_Daemon._on_caption, d)
    return d


def test_a_thought_goes_onto_the_screen_while_exploring() -> None:
    async def go():
        d = _exploring_daemon()
        await d._handle_ipc({"evt": "explore", "action": "start"})
        d._show_thought(_thought("Someone brought a plant to my desk.", tags=("plant",)))
        await asyncio.sleep(0)
        pages = _captions(d)
        assert pages, "nothing was drawn"
        first = pages[0]
        assert first["page"] == 0 and first["lines"]
        assert all(len(line) <= 17 for line in first["lines"])
        assert len(first["lines"]) <= 4
        assert "plant" in " ".join(p_line for p in pages for p_line in p.get("lines", []))
    asyncio.run(go())


def test_a_photographed_thought_says_so_on_the_screen() -> None:
    async def go():
        d = _exploring_daemon()
        await d._handle_ipc({"evt": "explore", "action": "start"})
        d._show_thought(_thought("A plant arrived.", tags=("plant",), photographed=True))
        await asyncio.sleep(0)
        text = " ".join(line for p in _captions(d) for line in p.get("lines", []))
        assert "photo" in text
    asyncio.run(go())


def test_nothing_is_drawn_when_the_screen_is_not_eves_to_use() -> None:
    async def go():
        # not exploring at all
        d = _exploring_daemon()
        d._show_thought(_thought("A thought nobody asked for."))
        assert _captions(d) == []

        # a conversation owns the screen
        d = _exploring_daemon()
        await d._handle_ipc({"evt": "explore", "action": "start"})
        d._conversation = _Conversation()
        d._show_thought(_thought("A thought."))
        assert _captions(d) == []

        # the owner is dictating
        d = _exploring_daemon()
        await d._handle_ipc({"evt": "explore", "action": "start"})
        d._listen_down = True
        d._show_thought(_thought("A thought."))
        assert _captions(d) == []

        # the board is away
        d = _exploring_daemon()
        await d._handle_ipc({"evt": "explore", "action": "start"})
        d.ble.connected = False
        d._show_thought(_thought("A thought."))
        assert _captions(d) == []
    asyncio.run(go())


def test_the_screen_is_cleared_when_the_explore_ends() -> None:
    async def go():
        d = _exploring_daemon()
        await d._handle_ipc({"evt": "explore", "action": "start"})
        d._show_thought(_thought("Half-read when the human walks in."))
        await asyncio.sleep(0)
        assert _captions(d)
        d._keys = SimpleNamespace(tap=lambda name: None)
        await d._handle_ble({"cmd": "key", "name": "enter"})     # a touch ends it
        await asyncio.sleep(0)                                  # _on_caption sends on a task
        assert {"cmd": "caption", "clear": True} in _captions(d)
        assert _captions(d)[-1] == {"cmd": "caption", "clear": True}
    asyncio.run(go())


# ---- being called back by hand ---------------------------------------------------------------

def test_a_double_tap_on_the_screen_calls_eve_back() -> None:
    async def go():
        d = _daemon()
        d._last_activity_at = 0.0
        await d._handle_ipc({"evt": "explore", "action": "start"})
        await d._handle_ble({"cmd": "explore", "stop": True})
        assert d._explorer.state == "off" and not d._explorer.manual
        assert d.ble.sent[-1] == MODE_OFF
        assert d._last_activity_at > 0.0                    # the human is here
        # ... and it does not resume on the next tick
        await d._explore_step(1.0)
        assert d._explorer.state == "off"
    asyncio.run(go())


def test_the_voice_pe_music_input_reaches_spotify_mode_or_puts_the_board_back() -> None:
    async def go():
        d = _daemon()
        got: list = []
        shown: list = []

        async def on_input(obj):
            got.append(obj)

        async def show(obj):
            shown.append(obj)
        d._controller = SimpleNamespace(show=show)
        await d._handle_ble({"cmd": "music", "clicks": 2})       # no Spotify: the board leaves the mode
        assert shown == [{"cmd": "music_mode", "on": False}] and got == []
        d._music_mode = SimpleNamespace(on_input=on_input)
        await d._handle_ble({"cmd": "music", "dial": 3})
        assert got == [{"cmd": "music", "dial": 3}]
    asyncio.run(go())
