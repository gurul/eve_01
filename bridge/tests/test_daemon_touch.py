"""A board touch (focus / key) while a voice conversation is open does nothing
but count as activity: no hush, no task cancel, no terminal raise, no key tap.
On 2026-09-10 a phantom body-pad blip in the attention pose sent focus and
killed a live task. Outside a conversation, focus and key behave as before.

No board, no desktop: the same SimpleNamespace + MethodType stub that
test_daemon_explore.py uses, with focus_session_terminal and KeyTapper patched.
"""

from __future__ import annotations

import asyncio
from pathlib import Path
from types import MethodType, SimpleNamespace

import pytest

from eve_bridge import focus_terminal, key_tap
from eve_bridge.caption_pager import CaptionPager
from eve_bridge.daemon import Daemon
from eve_bridge.explore import ExploreConfig, Explorer
from eve_bridge.thought_screen import ThoughtScreen


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


class _Agent:
    """A desktop task that is running."""

    def __init__(self) -> None:
        self.running = True
        self.cancel_reasons: list[str] = []

    def cancel(self, reason: str = "") -> None:
        self.cancel_reasons.append(reason)
        self.running = False


class _Keys:
    def __init__(self) -> None:
        self.taps: list[str] = []

    def tap(self, name: str) -> None:
        self.taps.append(name)


def _cfg() -> ExploreConfig:
    return ExploreConfig(enabled=True, after_secs=600.0, notes_per_hour=0.0, model="fake",
                         notes_dir=Path("/nonexistent"), cycle_wait_secs=900.0)


def _daemon() -> SimpleNamespace:
    d = SimpleNamespace(
        ble=_Ble(),
        state=SimpleNamespace(running_count=0, waiting_count=1,
                              attention_cwd=lambda: "/work/project"),
        _listen_sent=None,
        _listen_down=False,
        _explore_cfg=_cfg(),
        _explorer=Explorer(_cfg(), now=0.0, notes_enabled=False),
        _notes=None,
        _last_activity_at=0.0,
        _explore_raw_frame=None,
        _conversation=None,
        _active_agent=None,
        _agent_state="idle",
        _explore_after_conversation=None,
        _keys=None,
        _last_diag=None,
    )
    d._thought_pager = CaptionPager()
    d._screen = ThoughtScreen()
    for name in ("_note_activity", "_idle_secs", "_stop_explore", "_dismiss_explore",
                 "_clear_thought", "_flush_thought_pager", "_handle_ble", "_cancel_active_task",
                 "_on_caption"):
        setattr(d, name, MethodType(getattr(Daemon, name), d))
    return d


@pytest.fixture
def desktop(monkeypatch):
    """Record every terminal raise and every KeyTapper built; nothing reaches the desktop."""
    rec = SimpleNamespace(focused=[], tappers=[])

    async def fake_focus(cwd: str) -> None:
        rec.focused.append(cwd)

    def fake_tapper(*a, **kw):
        keys = _Keys()
        rec.tappers.append(keys)
        return keys

    monkeypatch.setattr(focus_terminal, "focus_session_terminal", fake_focus)
    monkeypatch.setattr(key_tap, "KeyTapper", fake_tapper)
    return rec


def _in_conversation(d: SimpleNamespace) -> tuple[_Conversation, _Agent]:
    conv, agent = _Conversation(), _Agent()
    d._conversation = conv
    d._active_agent = agent
    return conv, agent


def test_focus_during_a_conversation_is_ignored(desktop, caplog) -> None:
    async def go():
        d = _daemon()
        conv, agent = _in_conversation(d)
        with caplog.at_level("INFO"):
            await d._handle_ble({"cmd": "focus"})
        await asyncio.sleep(0)                      # a focus raise would run on a task
        assert not conv.cancelled
        assert agent.running and agent.cancel_reasons == []
        assert desktop.focused == []
        assert d._last_activity_at > 0.0          # the touch still counts as activity
        assert not any(m.get("cmd") == "caption" for m in d.ble.sent)   # no "stopped:" caption
        assert any("board touch (focus) during a conversation" in r.message for r in caplog.records)
        assert not any("hushed" in r.message for r in caplog.records)
    asyncio.run(go())


def test_key_during_a_conversation_taps_nothing(desktop) -> None:
    async def go():
        d = _daemon()
        conv, agent = _in_conversation(d)
        await d._handle_ble({"cmd": "key", "name": "enter"})
        assert not conv.cancelled
        assert agent.running and agent.cancel_reasons == []
        assert desktop.tappers == [] and d._keys is None
        assert d._last_activity_at > 0.0
    asyncio.run(go())


def test_a_closed_conversation_does_not_block_a_touch(desktop) -> None:
    async def go():
        d = _daemon()
        conv, _ = _in_conversation(d)
        conv.cancel()                               # done(): the conversation has ended
        await d._handle_ble({"cmd": "focus"})
        await asyncio.sleep(0)
        assert desktop.focused == ["/work/project"]
    asyncio.run(go())


def test_focus_outside_a_conversation_raises_the_terminal(desktop) -> None:
    async def go():
        d = _daemon()
        await d._handle_ble({"cmd": "focus"})
        await asyncio.sleep(0)
        assert desktop.focused == ["/work/project"]
        assert d._last_activity_at > 0.0
    asyncio.run(go())


def test_key_outside_a_conversation_taps_enter(desktop) -> None:
    async def go():
        d = _daemon()
        await d._handle_ble({"cmd": "key", "name": "enter"})
        assert len(desktop.tappers) == 1 and desktop.tappers[0].taps == ["enter"]
    asyncio.run(go())
