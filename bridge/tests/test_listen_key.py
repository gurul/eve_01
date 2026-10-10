"""Listen key — the debounced Option-hold tracker and the daemon's wire edge.

No Quartz here: the tracker is fed raw flag ints, and the daemon test drives
Daemon._on_listen_key against a stub transport the way test_folder_push does.
"""

from __future__ import annotations

import asyncio
from types import SimpleNamespace

import pytest

from eve_bridge import listen_key
from eve_bridge.key_tap import FLAG_ALTERNATE, FLAG_SECONDARY_FN
from eve_bridge.listen_key import (
    DEBOUNCE_SECS,
    ListenKeyTracker,
    configured_listen_key,
    start_listen_key,
)

OTHER_FLAGS = 0x00020000   # kCGEventFlagMaskShift — must never count as Option


def _tracker(key: str = "option") -> ListenKeyTracker:
    return ListenKeyTracker(key=key)


# ---- press / release --------------------------------------------------------

def test_press_then_release_reports_both_edges() -> None:
    t = _tracker()
    assert t.feed(FLAG_ALTERNATE, 0.0) is True
    assert t.feed(0, 1.0) is False


def test_same_state_is_never_reported_twice() -> None:
    t = _tracker()
    assert t.feed(FLAG_ALTERNATE, 0.0) is True
    assert t.feed(FLAG_ALTERNATE, 1.0) is None
    assert t.feed(FLAG_ALTERNATE | OTHER_FLAGS, 2.0) is None  # shift added, option still down


def test_other_modifiers_are_ignored() -> None:
    t = _tracker()
    assert t.feed(OTHER_FLAGS, 0.0) is None
    assert t.feed(0, 1.0) is None
    assert not t.down


def test_fn_key_uses_secondary_fn_mask() -> None:
    t = _tracker("fn")
    assert t.feed(FLAG_ALTERNATE, 0.0) is None
    assert t.feed(FLAG_SECONDARY_FN, 1.0) is True
    assert t.feed(0, 2.0) is False


# ---- debounce ---------------------------------------------------------------

def test_release_inside_window_is_held_then_flushed() -> None:
    """A quick tap must still end with an 'up' — losing it sticks the pose."""
    t = _tracker()
    assert t.feed(FLAG_ALTERNATE, 0.0) is True
    assert t.feed(0, 0.05) is None
    assert t.due_in(0.05) == pytest.approx(DEBOUNCE_SECS - 0.05)
    assert t.flush(0.10) is None            # window still open
    assert t.flush(DEBOUNCE_SECS) is False  # released at the window's end
    assert t.due_in(DEBOUNCE_SECS) is None


def test_chatter_that_settles_back_is_dropped() -> None:
    t = _tracker()
    assert t.feed(FLAG_ALTERNATE, 0.0) is True
    assert t.feed(0, 0.02) is None
    assert t.feed(FLAG_ALTERNATE, 0.04) is None   # back to what we reported
    assert t.due_in(0.04) is None
    assert t.flush(1.0) is None
    assert t.down is True


def test_at_most_one_edge_per_window() -> None:
    t = _tracker()
    edges = [t.feed(f, i * 0.01) for i, f in enumerate([FLAG_ALTERNATE, 0] * 5)]   # 0.09 s of bounce
    assert [e for e in edges if e is not None] == [True]
    assert t.flush(DEBOUNCE_SECS) is False


def test_edge_after_window_passes_immediately() -> None:
    t = _tracker()
    assert t.feed(FLAG_ALTERNATE, 0.0) is True
    assert t.feed(0, DEBOUNCE_SECS) is False


# ---- env selection / off ----------------------------------------------------

def test_env_selects_fn(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVE_LISTEN_KEY", " FN ")
    assert configured_listen_key() == "fn"
    assert ListenKeyTracker().mask == FLAG_SECONDARY_FN


def test_env_default_is_option_on_darwin(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.delenv("EVE_LISTEN_KEY", raising=False)
    monkeypatch.setattr(listen_key.sys, "platform", "darwin")
    assert configured_listen_key() == "option"
    monkeypatch.setattr(listen_key.sys, "platform", "linux")
    assert configured_listen_key() == "off"


def test_env_unknown_falls_back_to_default(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("EVE_LISTEN_KEY", "hyper")
    monkeypatch.setattr(listen_key.sys, "platform", "darwin")
    assert configured_listen_key() == "option"


def test_off_mode_reports_nothing() -> None:
    t = _tracker("off")
    assert not t.enabled
    assert t.feed(FLAG_ALTERNATE, 0.0) is None
    assert t.feed(0, 1.0) is None


def test_start_returns_none_when_off(monkeypatch: pytest.MonkeyPatch) -> None:
    """Disabled must short-circuit before any platform code runs."""
    monkeypatch.setenv("EVE_LISTEN_KEY", "off")
    calls: list[bool] = []
    assert start_listen_key(calls.append, asyncio.new_event_loop()) is None
    assert calls == []


def test_start_returns_none_off_darwin(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(listen_key.sys, "platform", "linux")
    assert start_listen_key(lambda _on: None, asyncio.new_event_loop(),
                            tracker=ListenKeyTracker(key="option")) is None


# ---- daemon wire edge -------------------------------------------------------

class _StubBle:
    def __init__(self, connected: bool = True) -> None:
        self.connected = connected
        self.sent: list[dict] = []

    async def send(self, obj: dict) -> bool:
        self.sent.append(obj)
        return True


def _daemon(connected: bool = True) -> SimpleNamespace:
    # _note_activity: the listen key is activity for the idle explorer (explore.py).
    return SimpleNamespace(ble=_StubBle(connected), _listen_sent=None, _note_activity=lambda: None)


async def _drive(d: SimpleNamespace, edges: list[bool]) -> list[dict]:
    from eve_bridge.daemon import Daemon

    for on in edges:
        Daemon._on_listen_key(d, on)
    await asyncio.sleep(0)   # let the send tasks run
    return d.ble.sent


def test_daemon_sends_once_per_transition() -> None:
    d = _daemon()
    sent = asyncio.run(_drive(d, [True, True, False, False, True]))
    assert sent == [
        {"cmd": "listen", "on": True},
        {"cmd": "listen", "on": False},
        {"cmd": "listen", "on": True},
    ]


def test_daemon_sends_nothing_while_disconnected() -> None:
    d = _daemon(connected=False)
    assert asyncio.run(_drive(d, [True, False])) == []
    assert d._listen_sent is None


def test_daemon_reset_on_connect_sends_off_and_rearms() -> None:
    from eve_bridge.daemon import Daemon

    async def go() -> list[dict]:
        d = _daemon()
        await _drive(d, [True])
        await Daemon._reset_listen(d)         # board rebooted mid-hold
        assert d._listen_sent is False
        await _drive(d, [False])              # release: board already knows
        await _drive(d, [True])
        return d.ble.sent

    assert asyncio.run(go()) == [
        {"cmd": "listen", "on": True},
        {"cmd": "listen", "on": False},
        {"cmd": "listen", "on": True},
    ]
