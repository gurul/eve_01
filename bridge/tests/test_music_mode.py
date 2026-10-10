"""music_mode.py: Spotify mode on the Voice PE — clicks, holds, the dial as volume, the device picker."""

from __future__ import annotations

import asyncio
from typing import Any

from eve_bridge import music_mode as M
from eve_bridge import spotify as S


class FakeSpotify:
    def __init__(self) -> None:
        self.playing = True
        self.volume = 40
        self.devices = [{"name": "MacBook", "type": "computer", "active": True, "volume": 40, "color": "#005aff"},
                        {"name": "Kitchen Speaker", "type": "speaker", "active": False, "color": "#ff6e00"},
                        {"name": "TV", "type": "tv", "active": False, "restricted": True}]
        self.calls: list[tuple[str, dict]] = []
        self.refuse: set[str] = set()

    async def now_playing(self) -> dict[str, Any]:
        return {"ok": True, "playing": self.playing, "track": "One More Time",
                "device": {"name": "MacBook", "volume": self.volume}, "devices": self.devices}

    async def control(self, action: str, **kw: Any) -> dict[str, Any]:
        self.calls.append((action, kw))
        if action in self.refuse:
            return {"ok": False, "line": "Controlling playback needs Spotify Premium."}
        if action == "volume":
            self.volume = kw["volume"]
        if action in ("play", "pause"):
            self.playing = action == "play"
        return {"ok": True, "line": f"did {action}" + (f" on {kw['device']}" if kw.get("device") else "")}


class Rig:
    def __init__(self, say: bool = True) -> None:
        self.spotify = FakeSpotify()
        self.sent: list[dict] = []
        self.said: list[str] = []
        self.sleeps: list[float] = []
        self.idle_passes = False           # the picker's 10 s idle timer fires only when a test says so

        async def send(obj: dict) -> None:
            self.sent.append(obj)

        async def speak(text: str) -> None:
            self.said.append(text)

        async def sleep(secs: float) -> None:      # short timers fire at once; the sleeps are recorded
            self.sleeps.append(secs)
            if secs >= M.PICKER_IDLE_SECS and not self.idle_passes:
                await asyncio.Event().wait()       # cancelled by the next input, or by close()
            await asyncio.sleep(0)
        self.mode = M.MusicMode(self.spotify, send, say=speak if say else None, sleep=sleep)

    async def settle(self) -> None:
        for _ in range(10):
            await asyncio.sleep(0)


def go(rig: Rig, *events: dict) -> None:
    async def run() -> None:
        await rig.mode.set(True)
        for e in events:
            await rig.mode.on_input({"cmd": "music", **e})
            await rig.settle()
        await rig.mode.close()
    asyncio.run(run())


def test_turning_it_on_tells_the_board_and_reads_the_volume() -> None:
    rig = Rig()

    async def run() -> tuple[str, str, str]:
        on = await rig.mode.set(True)
        again = await rig.mode.set(True)
        off = await rig.mode.set(False)
        return on, again, off
    on, again, off = asyncio.run(run())
    assert rig.sent == [{"cmd": "music_mode", "on": True}, {"cmd": "music_mode", "on": False}]
    assert "dial is the volume" in on and again == "Spotify mode is already on." and off == "Spotify mode off."


def test_one_click_toggles_two_skip_three_go_back() -> None:
    rig = Rig()
    go(rig, {"clicks": 1}, {"clicks": 1}, {"clicks": 2}, {"clicks": 3}, {"clicks": 5})
    assert [a for a, _ in rig.spotify.calls] == ["pause", "play", "next", "previous"]    # 5 clicks: nothing


def test_the_dial_is_the_volume_shown_at_once_written_once() -> None:
    rig = Rig()
    go(rig, {"dial": 1}, {"dial": 2}, {"dial": -1})
    levels = [o["n"] for o in rig.sent if o.get("cmd") == "ring_level"]
    assert levels == [44, 52, 48]                                  # 4% a detent, from Spotify's 40
    # every step starts the debounce again; each write that lands is the shadow at that moment
    assert [kw["volume"] for a, kw in rig.spotify.calls if a == "volume"][-1] == 48
    assert M.VOLUME_DEBOUNCE_SECS in rig.sleeps


def test_the_volume_is_clamped() -> None:
    rig = Rig()
    go(rig, {"dial": 30})
    assert [o["n"] for o in rig.sent if o.get("cmd") == "ring_level"] == [100]


def test_hold_opens_the_picker_dial_moves_click_plays_there() -> None:
    rig = Rig()
    go(rig, {"hold": "short"}, {"dial": 1}, {"clicks": 1})
    dots = [o for o in rig.sent if o.get("cmd") == "ring_level" and o.get("dot")]
    assert [(d["n"], d["of"]) for d in dots] == [(1, 2), (2, 2)]    # the TV (restricted) is not in the picker
    assert [d["rgb"] for d in dots] == [[0, 90, 255], [255, 110, 0]]  # each device in its own colour
    assert ("transfer", {"device": "Kitchen Speaker"}) in rig.spotify.calls
    assert {"cmd": "music_flash", "ok": True, "rgb": [255, 110, 0]} in rig.sent    # the sweep in its colour
    assert "Kitchen Speaker" in rig.said and rig.said[-1] == "did transfer on Kitchen Speaker"
    assert rig.mode.picker is None and rig.mode.on is True


def test_in_the_picker_a_click_on_the_playing_device_or_a_hold_changes_nothing() -> None:
    rig = Rig()
    go(rig, {"hold": "short"}, {"clicks": 1}, {"hold": "short"}, {"dial": 1}, {"hold": "short"})
    assert not any(a == "transfer" for a, _ in rig.spotify.calls)
    assert rig.mode.picker is None
    assert rig.sent[-1] == {"cmd": "ring_level", "n": 0, "of": 1, "ms": 0}         # the dot is cleared


def test_the_picker_times_out() -> None:
    rig = Rig()
    rig.idle_passes = True

    async def run() -> None:
        await rig.mode.set(True)
        await rig.mode.on_input({"cmd": "music", "hold": "short"})
        await rig.settle()
    asyncio.run(run())
    assert M.PICKER_IDLE_SECS in rig.sleeps and rig.mode.picker is None


def test_a_long_hold_leaves_the_mode() -> None:
    rig = Rig()
    go(rig, {"hold": "long"}, {"clicks": 1})
    assert rig.mode.on is False
    assert {"cmd": "music_mode", "on": False} in rig.sent and {"cmd": "music_flash", "ok": True} in rig.sent
    assert rig.spotify.calls == []                              # the click after it is not Spotify's any more


def test_a_refusal_blinks_red() -> None:
    rig = Rig()
    rig.spotify.refuse = {"next"}
    go(rig, {"clicks": 2})
    assert rig.sent[-1] == {"cmd": "music_flash", "ok": False}


def test_no_device_online_blinks_and_says_so() -> None:
    rig = Rig()
    rig.spotify.devices = []
    go(rig, {"hold": "short"})
    assert {"cmd": "music_flash", "ok": False} in rig.sent and rig.said == ["No Spotify device is online."]


def test_input_while_off_puts_the_board_back() -> None:
    rig = Rig()
    asyncio.run(rig.mode.on_input({"cmd": "music", "clicks": 1}))
    assert rig.sent == [{"cmd": "music_mode", "on": False}] and rig.spotify.calls == []


def test_without_a_voice_the_picker_is_the_ring_alone() -> None:
    rig = Rig(say=False)
    go(rig, {"hold": "short"}, {"dial": 1}, {"clicks": 1})
    assert rig.said == [] and ("transfer", {"device": "Kitchen Speaker"}) in rig.spotify.calls


# ---- saying it: spotify.py's mode commands ----

def test_spotify_mode_by_words_and_by_the_tool() -> None:
    h = S.Spotify(None)
    assert h.match("spotify mode") == S.Command("mode", on=True)
    assert h.match("turn off spotify mode") == S.Command("mode", on=False)
    assert h.match("exit spotify mode") == S.Command("mode", on=False)
    assert h.match("what is spotify mode") is None
    # without a Voice PE, it says so
    assert "Voice PE" in asyncio.run(h.run(S.Command("mode", on=True)))
    rig = Rig()
    h.mode = rig.mode
    assert asyncio.run(h.run(S.Command("mode", on=True))).startswith("Spotify mode on")
    args = {"action": "mode_off", "query": None, "kind": None, "volume": None, "volume_change": None, "device": None}
    assert asyncio.run(h.handle("spotify_control", args))["line"] == "Spotify mode off."
    assert rig.mode.on is False


# ---- the controller keeps the mode for a board that reconnects ----

def test_controller_show_keeps_the_mode_only() -> None:
    from eve_bridge.controller import INPUT_CMDS, Controller

    class Link:
        connected = True

        def __init__(self, **_kw: Any) -> None:
            self.sent: list = []
            self.on_boot = None

        async def send(self, obj: dict) -> None:
            self.sent.append(obj)

    async def on_input(_obj: dict) -> None:
        return None

    ctl = Controller("SERIAL", on_input, link_factory=Link)

    async def run() -> None:
        await ctl.show({"cmd": "music_mode", "on": True})
        await ctl.show({"cmd": "ring_level", "n": 40, "of": 100})
        await ctl._replay()
    asyncio.run(run())
    assert "music" in INPUT_CMDS
    assert ctl.last == {"music_mode": {"cmd": "music_mode", "on": True}}
    assert ctl.link.sent[-1] == {"cmd": "music_mode", "on": True}          # replayed on (re)connect


def test_a_device_without_a_colour_is_green() -> None:
    assert M._rgb({"name": "x"}) == [30, 215, 96] and M._rgb({"color": "#zzzzzz"}) == [30, 215, 96]
    assert M._rgb({"color": "#ff6e00"}) == [255, 110, 0]
