"""Spotify mode on the Voice PE: the board becomes spotKnob.

The owner asked on 2026-09-27: "when spotify mode is on voice pe leds should be green and the rotary board should
become volume", "pause on spotify is one click, skip is two clicks, back is three clicks", "change device is hold",
and exit on a long hold (3 s; two clicks were already skip). Turned on by saying or texting "spotify mode" (the
code path and the tools in spotify.py) or from the Mini App's Music card.

The firmware (firmware/eve_voice_pe) stays thin: in music mode it breathes green, counts clicks, times holds
and passes the dial through, as ``{"cmd":"music", ...}`` lines. Everything that needs Spotify is here:

==============================  ================================================================================
On the board                    What happens
==============================  ================================================================================
1 click                         play / pause
2 clicks                        next track
3 clicks                        previous track
dial                            Spotify volume, ``VOLUME_PER_DETENT``% a detent; the ring shows it in green at
                                once and the write goes to Spotify ``VOLUME_DEBOUNCE_SECS`` after the dial stops
                                (spotKnob's rule: one write per detent gets the account rate-limited)
hold, let go before 3 s         the device picker: the dial moves through the Spotify Connect devices (the ring
                                takes the device's own colour, spotify.DeviceColors, with a bright dot for its
                                place; eve says its name on the Voice PE), a click plays there, a hold or
                                ``PICKER_IDLE_SECS`` without input backs out, nothing changed
hold 3 s                        leave Spotify mode (the ring goes back to eve's)
==============================  ================================================================================

A refusal (no device, not Premium) blinks the ring red with the "no" chirp. Hold to talk and Enter-on-a-waiting-
prompt are the button's again once the mode is off. The mode does not survive a daemon restart.
"""
from __future__ import annotations

import asyncio
import contextlib
import logging
from typing import Any, Awaitable, Callable, Optional

log = logging.getLogger(__name__)

VOLUME_PER_DETENT = 4              # the Voice PE's dial has fewer, heavier detents than spotKnob's (2%)
VOLUME_DEBOUNCE_SECS = 0.4         # spotKnob's debounce
PICKER_IDLE_SECS = 10.0            # spotKnob's picker times out the same
PICKER_SAY_SECS = 0.35             # a name is said once the dial rests this long, not for every detent

Send = Callable[[dict[str, Any]], Awaitable[Any]]
Say = Callable[[str], Awaitable[Any]]


def _rgb(device: dict[str, Any]) -> list[int]:
    """A device's colour (spotify.DeviceColors, as ``"#rrggbb"`` on the device) as [r, g, b] for the ring."""
    c = str(device.get("color") or "")
    if len(c) == 7 and c.startswith("#"):
        try:
            return [int(c[i:i + 2], 16) for i in (1, 3, 5)]
        except ValueError:
            pass
    return [30, 215, 96]                                 # no colour: Spotify green


class MusicMode:
    """The Voice PE's Spotify mode: the board's input in, Spotify commands and ring levels out."""

    def __init__(self, spotify: Any, send: Send, say: Optional[Say] = None,
                 sleep: Callable[[float], Awaitable[Any]] = asyncio.sleep) -> None:
        self.spotify = spotify                    # spotify.Spotify
        self.send = send                          # to the Voice PE only (Controller.show)
        self.say = say                            # eve's voice on the Voice PE, or None (ring only)
        self._sleep = sleep
        self.on = False
        self.volume: Optional[int] = None         # the shadow the dial moves; Spotify's own read when the mode opens
        self._volume_task: Optional[asyncio.Task] = None
        self.picker: Optional[list[dict[str, Any]]] = None   # the devices while the picker is open
        self.index = 0
        self._picker_timer: Optional[asyncio.Task] = None
        self._say_task: Optional[asyncio.Task] = None
        self._tasks: set[asyncio.Task] = set()

    # -- on and off --
    async def set(self, on: bool) -> str:
        """Turn the mode on or off; the line to tell the owner."""
        if on == self.on:
            return "Spotify mode is already " + ("on." if on else "off.")
        self.on = on
        await self._close_picker()
        await self.send({"cmd": "music_mode", "on": on})
        log.info("music mode: %s", "on" if on else "off")
        if on:
            now = await self.spotify.now_playing()
            vol = (now.get("device") or {}).get("volume")
            self.volume = vol if isinstance(vol, int) else None
            return ("Spotify mode on: the dial is the volume, one click plays or pauses, two skip, three go back, "
                    "hold to pick a device, hold 3 s to leave.")
        return "Spotify mode off."

    # -- the board's input --
    async def on_input(self, obj: dict[str, Any]) -> None:
        """``{"cmd":"music","clicks":n}``, ``{"cmd":"music","hold":"short"|"long"}``, ``{"cmd":"music","dial":d}``."""
        if not self.on:
            # a board that still thinks it is in music mode (the daemon restarted): put it back
            await self.send({"cmd": "music_mode", "on": False})
            return
        if obj.get("hold") == "long":
            await self.set(False)
            await self._flash("done")
            return
        if self.picker is not None:
            await self._picker_input(obj)
            return
        if obj.get("hold") == "short":
            await self._open_picker()
            return
        dial = obj.get("dial")
        if isinstance(dial, int) and not isinstance(dial, bool) and dial:
            await self._dial_volume(dial)
            return
        clicks = obj.get("clicks")
        if clicks in (1, 2, 3):
            action = {1: "", 2: "next", 3: "previous"}[clicks]
            if not action:
                now = await self.spotify.now_playing()
                action = "pause" if now.get("playing") else "play"
            out = await self.spotify.control(action)
            log.info("music mode: %d click(s) -> %s (%s)", clicks, action, "done" if out.get("ok") else "refused")
            if not out.get("ok"):
                await self._flash("error")

    async def _flash(self, state: str, rgb: Optional[list[int]] = None) -> None:
        """A short sweep ("done": green, or ``rgb``) or red blink ("error") on the ring, with its chirp; the board
        times it."""
        with contextlib.suppress(Exception):
            await self.send({"cmd": "music_flash", "ok": state == "done", **({"rgb": rgb} if rgb else {})})

    # -- volume --
    async def _dial_volume(self, detents: int) -> None:
        if self.volume is None:
            now = await self.spotify.now_playing()
            vol = (now.get("device") or {}).get("volume")
            if not isinstance(vol, int):
                await self._flash("error")               # nothing playing anywhere: no volume to move
                return
            self.volume = vol
        self.volume = max(0, min(100, self.volume + detents * VOLUME_PER_DETENT))
        await self.send({"cmd": "ring_level", "n": self.volume, "of": 100})
        if self._volume_task is not None and not self._volume_task.done():
            self._volume_task.cancel()
        self._volume_task = self._spawn(self._write_volume())

    async def _write_volume(self) -> None:
        await self._sleep(VOLUME_DEBOUNCE_SECS)
        out = await self.spotify.control("volume", volume=self.volume)
        if not out.get("ok"):
            self.volume = None                           # read it again next time
            await self._flash("error")

    # -- the device picker --
    async def _open_picker(self) -> None:
        now = await self.spotify.now_playing()
        devices = [d for d in now.get("devices") or [] if not d.get("restricted")] if now.get("ok") else []
        if not devices:
            await self._flash("error")
            if self.say is not None:
                self._say_later("No Spotify device is online.", 0)
            return
        self.picker = devices
        self.index = next((i for i, d in enumerate(devices) if d.get("active")), 0)
        log.info("music mode: picker open, %d devices", len(devices))
        await self._show_pick()
        self._arm_picker_timer()

    async def _picker_input(self, obj: dict[str, Any]) -> None:
        assert self.picker is not None
        dial = obj.get("dial")
        if isinstance(dial, int) and not isinstance(dial, bool) and dial:
            self.index = (self.index + dial) % len(self.picker)
            await self._show_pick()
            self._arm_picker_timer()
            return
        if obj.get("hold") == "short":
            await self._close_picker()                   # back out, nothing changed
            return
        if obj.get("clicks") == 1:
            chosen = self.picker[self.index]
            await self._close_picker()
            if chosen.get("active"):
                return                                   # a click with no turn changes nothing, as on the knob
            out = await self.spotify.control("transfer", device=chosen["name"])
            log.info("music mode: picked %s (%s)", chosen["name"], "done" if out.get("ok") else "refused")
            if out.get("ok"):
                self.volume = None                       # the new device has its own volume
                await self._flash("done", _rgb(chosen))  # the sweep in the new device's colour
            else:
                await self._flash("error")
            if self.say is not None:
                self._say_later(str(out.get("line") or ""), 0)

    async def _show_pick(self) -> None:
        assert self.picker is not None
        await self.send({"cmd": "ring_level", "n": self.index + 1, "of": len(self.picker), "dot": True,
                         "ms": int(PICKER_IDLE_SECS * 1000), "rgb": _rgb(self.picker[self.index])})
        if self.say is not None:
            self._say_later(self.picker[self.index]["name"], PICKER_SAY_SECS)

    def _say_later(self, text: str, after: float) -> None:
        if self._say_task is not None and not self._say_task.done():
            self._say_task.cancel()                      # a newer name replaces one not yet said

        async def go() -> None:
            await self._sleep(after)
            assert self.say is not None
            with contextlib.suppress(Exception):
                await self.say(text)
        self._say_task = self._spawn(go())

    def _arm_picker_timer(self) -> None:
        if self._picker_timer is not None and not self._picker_timer.done():
            self._picker_timer.cancel()

        async def expire() -> None:
            await self._sleep(PICKER_IDLE_SECS)
            log.info("music mode: picker timed out")
            self._picker_timer = None                    # this task is ending: _close_picker must not cancel it
            await self._close_picker()
        self._picker_timer = self._spawn(expire())

    async def _close_picker(self) -> None:
        if self._picker_timer is not None and not self._picker_timer.done():
            self._picker_timer.cancel()
        self._picker_timer = None
        if self.picker is not None:
            self.picker = None
            with contextlib.suppress(Exception):
                await self.send({"cmd": "ring_level", "n": 0, "of": 1, "ms": 0})   # the dot goes

    def _spawn(self, coro: Awaitable[Any]) -> asyncio.Task:
        task = asyncio.ensure_future(coro)
        self._tasks.add(task)
        task.add_done_callback(self._tasks.discard)
        return task

    async def close(self) -> None:
        for t in list(self._tasks):
            t.cancel()
        await asyncio.gather(*self._tasks, return_exceptions=True)
