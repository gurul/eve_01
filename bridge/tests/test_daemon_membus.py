"""The daemon on its memory bus: what it publishes (state, lesson, diary), what it never publishes (the
learner's words, anything said in a conversation), the opt-in sinks, the recall service and the remember line."""

from __future__ import annotations

import asyncio
import logging
from pathlib import Path
from types import MethodType, SimpleNamespace

import pytest

from eve_bridge.caption_pager import CaptionPager
from eve_bridge.daemon import Daemon
from eve_bridge.diary import DiaryTaker, Record
from eve_bridge.explore import ExploreConfig, Explorer
from eve_bridge.memory_bus import BusConfig, MemoryBus, configured
from eve_bridge.thought_screen import ThoughtScreen


class _Ble:
    def __init__(self, connected: bool = True) -> None:
        self.connected = connected
        self.sent: list[dict] = []

    async def send(self, obj: dict, codec=None) -> bool:
        self.sent.append(obj)
        return True


def _cfg() -> ExploreConfig:
    return ExploreConfig(enabled=True, after_secs=600.0, notes_per_hour=0.0, model="fake",
                         notes_dir=Path("/nonexistent"), cycle_wait_secs=900.0)


def _daemon(learning=None, bus_cfg: BusConfig | None = None) -> SimpleNamespace:
    """The stub-daemon shape of test_daemon_lesson.py, plus the bus."""
    d = SimpleNamespace(
        ble=_Ble(True),
        state=SimpleNamespace(running_count=0, waiting_count=0),
        _listen_sent=None, _listen_down=False, _explore_cfg=_cfg(), _last_activity_at=0.0,
        _explore_raw_frame=None, _conversation=None, _agent_state="idle",
        _sound=SimpleNamespace(on=True, muted=False), _notes=None, _learning_idle_handle=None,
        _learning_notices=0, _lesson_action="",
        _learning_server=None if learning is None else SimpleNamespace(app=learning),
        bus=MemoryBus(), _bus_cfg=bus_cfg or BusConfig(),
        _rosbridge=None, _claude_mem_sink=None, _claude_mem_mirror=None,
    )
    d._explorer = Explorer(d._explore_cfg, now=0.0, notes_enabled=False)
    d._thought_pager = CaptionPager()
    d._screen = ThoughtScreen()
    for name in ("_handle_ipc", "_handle_lesson", "_learning_notice", "_learning_idle", "_on_agent_state",
                 "_on_caption", "_note_activity", "_dismiss_explore", "_clear_thought", "_run_explore_action",
                 "_request_explore", "_start_memory_sinks", "_stop_memory_sinks", "_publish_observation",
                 "_publish_lesson", "_on_remember", "_recall_service"):
        setattr(d, name, MethodType(getattr(Daemon, name), d))
    return d


def _events(bus: MemoryBus, topic: str) -> list[dict]:
    return [e.msg for e in bus.recent(topic)]


# ---- configured() ------------------------------------------------------------------------------

def test_everything_is_off_by_default() -> None:
    assert configured({}) == BusConfig()


def test_env_flags_and_mirror_default() -> None:
    cfg = configured({"EVE_ROSBRIDGE": "1", "EVE_ROSBRIDGE_PORT": "9999",
                      "EVE_ROSBRIDGE_HOST": "0.0.0.0", "EVE_CLAUDE_MEM": "true",
                      "EVE_CLAUDE_MEM_URL": "http://127.0.0.1:37701"})
    assert cfg == BusConfig(rosbridge=True, rosbridge_host="0.0.0.0", rosbridge_port=9999, claude_mem=True,
                            claude_mem_url="http://127.0.0.1:37701", mirror=True)
    assert configured({"EVE_CLAUDE_MEM": "on", "EVE_CLAUDE_MEM_MIRROR": "off"}).mirror is False
    assert configured({"EVE_ROSBRIDGE": "1", "EVE_ROSBRIDGE_PORT": "lots"}).rosbridge_port == 9090


# ---- emits -------------------------------------------------------------------------------------

def test_a_state_change_publishes_eve_state_once_per_change() -> None:
    async def go():
        d = _daemon()
        d._on_agent_state("thinking")
        d._on_agent_state("thinking")
        d._on_agent_state("speaking")
        await asyncio.sleep(0)
        states = [e["state"] for e in _events(d.bus, "/eve/state")]
        assert states == ["thinking", "speaking"]
        assert all(isinstance(e["time"], float) for e in _events(d.bus, "/eve/state"))
    asyncio.run(go())


class _Store:
    def __init__(self, lesson: dict) -> None:
        self.lesson = lesson

    def get(self, key):
        assert key == self.lesson["id"]
        return self.lesson

    @staticmethod
    def summary(s):
        return {k: s[k] for k in ("id", "topic", "level", "mode", "problem", "stage", "updated", "demo", "revision")}


def _learning_app(calls: list | None = None):
    lesson = {"id": "abc", "topic": "Fractions", "level": "Grade 4", "mode": "learn",
              "problem": "What is 1/2 + 1/4?", "stage": "working", "updated": 1.0, "demo": True, "revision": 3,
              "ideas": "I think I add the tops", "spoken": [{"rev": 2, "text": "maybe the bottoms too"}],
              "strokes": [{"tool": "pen", "points": [[0.1, 0.2]]}], "board_image": "data:image/png;base64,xx"}

    def voice(**kw):
        if calls is not None:
            calls.append(kw)
        return {"ok": True, "answer": "What do the bottoms need to be?", "lesson": _Store.summary(lesson)}
    return SimpleNamespace(voice=voice, active="abc", store=_Store(lesson))


def test_a_lesson_notice_publishes_the_lesson_without_the_learner_words() -> None:
    async def go():
        d = _daemon(learning=_learning_app())
        d._learning_notice("What do the bottoms need to be?", "working")
        await asyncio.sleep(0)
        events = _events(d.bus, "/eve/memory/lesson")
        assert len(events) == 1
        e = events[0]
        assert e["action"] == "notice" and e["stage"] == "working"
        assert e["topic"] == "Fractions" and e["level"] == "Grade 4" and e["mode"] == "learn"
        assert e["lesson_id"] == "abc" and e["feedback"] == "What do the bottoms need to be?"
        for forbidden in ("ideas", "spoken", "strokes", "board_image", "problem"):
            assert forbidden not in e
        text = repr(e)
        assert "add the tops" not in text and "bottoms too" not in text
        d._learning_idle_handle.cancel()
    asyncio.run(go())


def test_an_ipc_lesson_action_names_the_action_in_its_event_and_clears_it_after() -> None:
    async def go():
        calls: list = []
        d = _daemon(learning=_learning_app(calls))
        resp = await d._handle_ipc({"evt": "lesson", "action": "hint"})
        await asyncio.sleep(0)
        assert resp["ok"] and calls == [{"action": "hint"}]
        events = _events(d.bus, "/eve/memory/lesson")
        assert [e["action"] for e in events] == ["hint"]
        assert d._lesson_action == ""
        d._learning_idle_handle.cancel()
    asyncio.run(go())


def test_a_lesson_notice_without_a_learning_server_still_publishes_a_bare_event() -> None:
    async def go():
        d = _daemon()
        d._learning_notice("Try again.", "error")
        await asyncio.sleep(0)
        e = _events(d.bus, "/eve/memory/lesson")[0]
        assert e["action"] == "notice" and e["stage"] == "error" and e["topic"] == "" and e["feedback"] == "Try again."
        d._learning_idle_handle.cancel()
    asyncio.run(go())


def test_a_kept_diary_thought_publishes_an_observation_and_a_candidate_does_not(tmp_path) -> None:
    published: list[Record] = []
    taker = DiaryTaker(client=SimpleNamespace(), notes_dir=tmp_path, on_written=published.append)
    assert taker.on_written is not None and DiaryTaker(client=SimpleNamespace(), notes_dir=tmp_path).on_written is None
    d = _daemon()
    rec = Record(id=1, ts=1700000000.0, weekday=1, hour=15, yaw=0, pitch=0, thought="the chair is pushed in",
                 observations=["chair pushed in"], changed=["chair"], tags=["chair", "desk"], novelty=7,
                 importance=8, written=True, photo="photos/x.jpg", thumb="AAAA")
    d._publish_observation(rec)
    e = _events(d.bus, "/eve/memory/observation")[0]
    assert e == {"thought": "the chair is pushed in", "observations": ["chair pushed in"], "changed": ["chair"],
                 "tags": ["chair", "desk"], "importance": 8, "novelty": 7, "time": 1700000000.0}
    assert "photo" not in e and "thumb" not in e


def test_the_diary_only_calls_on_written_for_records_it_wrote(tmp_path, monkeypatch) -> None:
    """The write gate decides; a kept-in-memory candidate must not reach the bus."""
    import json

    from eve_bridge import diary as diary_mod
    from eve_bridge.explore import Note
    from eve_bridge.vision import Frame
    written: list[str] = []

    class _Client:
        n = 0

        def think(self, image: bytes, mime: str, context: str) -> str:
            _Client.n += 1
            thoughts = ["the blue mug moved to the window", "someone left the red lamp on tonight"]
            return json.dumps({"observations": ["a mug"], "changed": [], "thought": thoughts[_Client.n - 1],
                               "tags": ["mug"], "novelty": 2, "importance": 2,
                               "valence": 0, "arousal": 0, "label": "calm"})
    taker = DiaryTaker(client=_Client(), notes_dir=tmp_path, on_written=lambda r: written.append(r.thought))
    verdict = {"write": False}
    monkeypatch.setattr(diary_mod, "should_write", lambda *a, **k: verdict["write"])
    monkeypatch.setattr(taker, "_reflection_due", lambda when: False)

    async def consider_photo(*a, **k):
        return False
    monkeypatch.setattr(taker, "_consider_photo", consider_photo)
    frame = Frame(seq=1, w=32, h=24, fmt="gray", data=bytes(32 * 24))

    async def go():
        await taker.take(Note(frame, 0, 0))      # gate says no: memory only
        assert taker.taken == 1 and written == []
        verdict["write"] = True
        await taker.take(Note(frame, 0, 0))      # gate says yes: the bus hears it
        assert taker.taken == 2 and written == ["someone left the red lamp on tonight"]
    asyncio.run(go())


def test_nothing_said_in_a_conversation_is_published() -> None:
    """The conversation topic is gone (owner, 2026-09-23): what was said stays in the memory folder."""
    from eve_bridge.memory_bus import TOPICS

    assert "/eve/memory/conversation" not in TOPICS
    assert not hasattr(Daemon, "_publish_conversation")
    assert "/eve/memory/observation" in TOPICS            # the control: the diary topic is still there


# ---- sinks -------------------------------------------------------------------------------------

class _FakeServer:
    instances: list = []

    def __init__(self, bus, host="127.0.0.1", port=9090, queue_size=64) -> None:
        self.bus, self.host, self.port = bus, host, port
        self.url = f"ws://{host}:{port}"
        self.started = self.stopped = False
        _FakeServer.instances.append(self)

    async def start(self) -> None:
        self.started = True

    async def stop(self) -> None:
        self.stopped = True


class _FakeSink:
    instances: list = []

    def __init__(self, bus, project="eve", url=None, queue_size=256, timeout=5.0) -> None:
        self.bus, self.project, self.url = bus, project, url or "http://127.0.0.1:37701"
        self.started = self.stopped = False
        _FakeSink.instances.append(self)

    def start(self) -> None:
        self.started = True

    def stop(self, wait=2.0) -> None:
        self.stopped = True


class _FakeMirror(_FakeSink):
    def __init__(self, bus, url=None, reconnect_secs=5.0) -> None:
        super().__init__(bus, url=url)


def _install_fakes(monkeypatch) -> None:
    import sys
    import types
    rb = types.ModuleType("eve_bridge.rosbridge")
    rb.RosbridgeServer = _FakeServer
    cm = types.ModuleType("eve_bridge.claude_mem")
    cm.ClaudeMemSink = _FakeSink
    cm.ClaudeMemMirror = _FakeMirror
    cm.recall = lambda query, limit=5, url=None, timeout=5.0, project="eve": [{"id": 1, "title": query, "time": "now"}]
    monkeypatch.setitem(sys.modules, "eve_bridge.rosbridge", rb)
    monkeypatch.setitem(sys.modules, "eve_bridge.claude_mem", cm)
    _FakeServer.instances.clear()
    _FakeSink.instances.clear()


def test_with_the_flags_off_no_sink_starts_and_the_boot_line_says_so(monkeypatch, caplog) -> None:
    _install_fakes(monkeypatch)

    async def go():
        d = _daemon()
        with caplog.at_level(logging.INFO):
            await d._start_memory_sinks()
        assert _FakeServer.instances == [] and _FakeSink.instances == []
        assert d._rosbridge is None and d._claude_mem_sink is None and d._claude_mem_mirror is None
        assert any("memory bus: rosbridge off, claude-mem off, mirror off" in r.getMessage() for r in caplog.records)
        await d._stop_memory_sinks()
    asyncio.run(go())


def test_with_the_flags_on_the_sinks_start_and_stop(monkeypatch, caplog) -> None:
    _install_fakes(monkeypatch)

    async def go():
        d = _daemon(bus_cfg=BusConfig(rosbridge=True, rosbridge_port=9091, claude_mem=True,
                                      claude_mem_url="http://127.0.0.1:37701", mirror=True))
        with caplog.at_level(logging.INFO):
            await d._start_memory_sinks()
        server, = _FakeServer.instances
        sink, mirror = _FakeSink.instances
        assert server.started and server.port == 9091 and sink.started and mirror.started
        assert isinstance(mirror, _FakeMirror) and sink.project == "eve"
        assert any("memory bus: rosbridge ws://127.0.0.1:9091, claude-mem http://127.0.0.1:37701, mirror on"
                   in r.getMessage() for r in caplog.records)
        await d._stop_memory_sinks()
        assert server.stopped and sink.stopped and mirror.stopped
        assert d._rosbridge is None and d._claude_mem_sink is None and d._claude_mem_mirror is None
    asyncio.run(go())


def test_a_sink_that_cannot_start_is_logged_and_the_daemon_goes_on(monkeypatch, caplog) -> None:
    _install_fakes(monkeypatch)

    class _Busy(_FakeServer):
        async def start(self) -> None:
            raise OSError(48, "address in use")
    import sys
    sys.modules["eve_bridge.rosbridge"].RosbridgeServer = _Busy

    async def go():
        d = _daemon(bus_cfg=BusConfig(rosbridge=True, claude_mem=True))
        with caplog.at_level(logging.INFO):
            await d._start_memory_sinks()
        assert d._rosbridge is None and d._claude_mem_sink is not None
        assert any("rosbridge did not start" in r.getMessage() for r in caplog.records)
        await d._stop_memory_sinks()
    asyncio.run(go())


def test_missing_modules_are_a_warning_not_a_crash(monkeypatch, caplog) -> None:
    import sys
    monkeypatch.setitem(sys.modules, "eve_bridge.rosbridge", None)   # import raises ImportError
    monkeypatch.setitem(sys.modules, "eve_bridge.claude_mem", None)

    async def go():
        d = _daemon(bus_cfg=BusConfig(rosbridge=True, claude_mem=True))
        with caplog.at_level(logging.WARNING):
            await d._start_memory_sinks()
        assert d._rosbridge is None and d._claude_mem_sink is None
        messages = [r.getMessage() for r in caplog.records]
        assert any("rosbridge did not start" in m for m in messages)
        assert any("claude-mem did not start" in m for m in messages)
    asyncio.run(go())


# ---- recall and remember -------------------------------------------------------------------------

def test_recall_says_claude_mem_is_off_when_it_is(monkeypatch) -> None:
    _install_fakes(monkeypatch)

    async def go():
        d = _daemon()
        await d._start_memory_sinks()
        out = await d.bus.call_service("/eve/memory/recall", {"query": "mug"})
        assert out == {"results": [], "error": "claude-mem is off (EVE_CLAUDE_MEM=1)"}
    asyncio.run(go())


def test_recall_searches_claude_mem_when_it_is_on(monkeypatch) -> None:
    _install_fakes(monkeypatch)

    async def go():
        d = _daemon(bus_cfg=BusConfig(claude_mem=True, mirror=False))
        await d._start_memory_sinks()
        out = await d.bus.call_service("/eve/memory/recall", {"query": "mug", "limit": "2"})
        assert out == {"results": [{"id": 1, "title": "mug", "time": "now"}]}
        await d._stop_memory_sinks()
    asyncio.run(go())


def test_a_remember_on_the_bus_is_dropped_while_memory_is_off(monkeypatch, tmp_path, caplog) -> None:
    _install_fakes(monkeypatch)

    async def go():
        d = _daemon()
        d._memory = None
        await d._start_memory_sinks()
        with caplog.at_level(logging.INFO, logger="eve_bridge.daemon"):
            d.bus.publish("/eve/memory/remember", {"text": "The green mug lives on the left shelf."})
            await asyncio.sleep(0)
        assert "remember dropped" in caplog.text and "left shelf" not in caplog.text
        assert list(tmp_path.rglob("*")) == []
    asyncio.run(go())


@pytest.mark.parametrize("state", ["wake", "idle"])
def test_the_bus_is_optional_on_old_stubs(state: str) -> None:
    """Test stubs elsewhere build a daemon without a bus; _on_agent_state must not need one."""
    async def go():
        d = _daemon()
        del d.bus
        d._on_agent_state(state)
        await asyncio.sleep(0)
        assert d._agent_state == state
    asyncio.run(go())
