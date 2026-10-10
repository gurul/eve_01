"""The Mac microphone follows the robot: open while it is connected, closed when it
is not, and never open while the owner's switch is off (`eve-bridge mic off`)."""

from __future__ import annotations

import asyncio
import logging
import sys
from types import MethodType, SimpleNamespace

import numpy as np

from eve_bridge.cli import describe_mic
from eve_bridge.daemon import Daemon
from eve_bridge.ears import Ears, EarsConfig, configured
from eve_bridge.sound import SoundSetting

# ---- a sounddevice stand-in ------------------------------------------------------

class _Stream:
    opened = 0
    closed = 0

    def __init__(self, **kw) -> None:
        self.kw = kw

    def start(self) -> None:
        _Stream.opened += 1

    def stop(self) -> None:
        pass

    def close(self) -> None:
        _Stream.closed += 1


class _FakeSD:
    InputStream = _Stream
    default = SimpleNamespace(device=(0, 0))

    @staticmethod
    def query_devices(device=None, kind=None):
        return {"name": "Fake Mic"}


class _Spotter:
    def feed(self, samples, rate):
        return None


def _ears(monkeypatch) -> Ears:
    monkeypatch.setitem(sys.modules, "sounddevice", _FakeSD)
    _Stream.opened = _Stream.closed = 0
    return Ears(EarsConfig(), lambda _k: None, asyncio.new_event_loop(), spotter=_Spotter())


# ---- config ----------------------------------------------------------------------

def test_mic_always_is_off_by_default_and_reads_the_env() -> None:
    assert configured({}).always is False
    assert configured({"EVE_MIC_ALWAYS": "1"}).always is True
    assert configured({"EVE_MIC_ALWAYS": "off"}).always is False


# ---- Ears: prepare, start, stop ---------------------------------------------------

def test_prepare_loads_nothing_into_the_mic(monkeypatch) -> None:
    e = _ears(monkeypatch)
    assert e.prepare() is True
    assert not e.listening and _Stream.opened == 0


def test_start_is_idempotent_and_stop_closes(monkeypatch) -> None:
    e = _ears(monkeypatch)
    assert e.start() is True and e.listening
    assert e.start() is True                      # second call: still one stream
    assert _Stream.opened == 1
    e.stop()
    assert not e.listening and _Stream.closed == 1
    e.stop()                                      # idempotent too
    assert _Stream.closed == 1
    assert e.start() is True and _Stream.opened == 2   # reopens after a stop


def test_stop_resets_the_silence_watch(monkeypatch) -> None:
    e = _ears(monkeypatch)
    e.start()
    e._on_block(np.zeros(2400, dtype=np.int16), now=0.0)
    assert e._silent_since == 0.0
    e.stop()
    assert e._silent_since is None and e._silence_warned is False


# ---- the daemon's policy ----------------------------------------------------------

class _FakeEars:
    def __init__(self, can_open: bool = True) -> None:
        self.listening = False
        self.can_open = can_open
        self.events: list[str] = []

    def start(self) -> bool:
        self.events.append("start")
        self.listening = self.can_open
        return self.can_open

    def stop(self) -> None:
        self.events.append("stop")
        self.listening = False


def _daemon(tmp_path, connected: bool = False, always: bool = False, ears=None) -> SimpleNamespace:
    d = SimpleNamespace(
        ble=SimpleNamespace(connected=connected),
        _ears=_FakeEars() if ears is None else ears,
        _ears_cfg=EarsConfig(always=always),
        _mic=SoundSetting(tmp_path / "mic.json", name="mic"),
        _note_activity=lambda: None,
        _last_activity_at=0.0,
    )
    d._mic.load()
    for name in ("_mic_wanted", "_apply_mic", "_mic_status", "_handle_ipc"):
        setattr(d, name, MethodType(getattr(Daemon, name), d))
    return d


def test_the_mic_stays_closed_until_the_robot_connects(tmp_path) -> None:
    d = _daemon(tmp_path)
    d._apply_mic("boot")
    assert d._ears.events == [] and not d._ears.listening
    d.ble.connected = True
    d._apply_mic("the robot connected")
    assert d._ears.events == ["start"] and d._ears.listening
    d._apply_mic("the robot connected")            # a second connect event: no second open
    assert d._ears.events == ["start"]
    d.ble.connected = False
    d._apply_mic("the robot is not connected")
    assert d._ears.events == ["start", "stop"] and not d._ears.listening


def test_mic_always_opens_at_boot_and_survives_a_drop(tmp_path) -> None:
    d = _daemon(tmp_path, always=True)
    d._apply_mic("boot")
    assert d._ears.listening
    d.ble.connected = True
    d._apply_mic("the robot connected")
    d.ble.connected = False
    d._apply_mic("the robot is not connected")
    assert d._ears.events == ["start"] and d._ears.listening


def test_the_owners_switch_outranks_the_robot(tmp_path) -> None:
    async def go():
        d = _daemon(tmp_path, connected=True)
        d._apply_mic("boot")
        assert d._ears.listening
        r = await d._handle_ipc({"evt": "mic", "action": "off"})
        assert r == {"ok": True, "mic": "off", "listening": False, "connected": True, "always": False,
                     "available": True}
        assert d._ears.events == ["start", "stop"]
        assert SoundSetting(tmp_path / "mic.json", name="mic").load() is False   # a restart keeps it
        # the robot reconnecting does not reopen it
        d._apply_mic("the robot connected")
        assert not d._ears.listening
        r = await d._handle_ipc({"evt": "mic", "action": "on"})
        assert r["mic"] == "on" and r["listening"] is True
        assert d._ears.events == ["start", "stop", "start"]
    asyncio.run(go())


def test_mic_status_with_no_ears(tmp_path) -> None:
    async def go():
        d = _daemon(tmp_path, ears=False)
        d._ears = None
        d._apply_mic("boot")                          # no ears: nothing to do, nothing raised
        r = await d._handle_ipc({"evt": "mic", "action": "status"})
        assert r["available"] is False and r["listening"] is False
    asyncio.run(go())


def test_a_mic_that_will_not_open_is_one_warning(tmp_path, caplog) -> None:
    d = _daemon(tmp_path, connected=True, ears=_FakeEars(can_open=False))
    with caplog.at_level(logging.WARNING):
        d._apply_mic("the robot connected")
    assert "did not open" in caplog.text and not d._ears.listening


# ---- the CLI's one line -----------------------------------------------------------

def test_describe_mic() -> None:
    base = {"ok": True, "mic": "on", "listening": False, "connected": False, "always": False, "available": True}
    assert describe_mic({**base, "available": False}).startswith("eve has no microphone")
    assert describe_mic({**base, "mic": "off"}).startswith("microphone off (your choice)")
    assert describe_mic({**base, "listening": True}) == "microphone on: listening for the wake word"
    assert "EVE_MIC_ALWAYS" in describe_mic({**base, "listening": True, "always": True})
    assert describe_mic(base) == "microphone closed: it opens when the robot connects"
    assert "could not be opened" in describe_mic({**base, "connected": True})
