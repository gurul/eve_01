"""The controller: a second board, next to the robot, that only takes input and shows state.

The owner asked on 2026-09-26 for the StackChan to stay the robot (face, head, camera) while a Home Assistant
Voice PE (firmware/eve_voice_pe, docs/voice-pe.md) becomes the control: hold its button to talk (desk_call.py),
tap it for Enter on a waiting prompt, turn its dial through the choices. Both boards are ESP32-S3s on the same
``/dev/cu.usbmodem*`` glob, so the controller is named by its USB serial number, ``EVE_CONTROLLER_SERIAL``
(an ESP32-S3's is its MAC, printed by ``python -m serial.tools.list_ports -v``), and the robot's glob skips it.

The robot link stays the daemon's one link (``Daemon.ble``): every status poll, ack, watchdog and resync is the
robot's. The controller gets a copy of what it can show — the heartbeat, the time, the conversation state and
the sound setting — and its lines go back to the daemon only when they are input: ``ptt``, ``key``, ``focus``.
Its acks never reach the robot's ack waiters.
"""
from __future__ import annotations

import asyncio
import contextlib
import logging
import os
from typing import Any, Awaitable, Callable, Mapping, Optional

from .serial_transport import USB_SERIAL_PREFIX, EveSerial

log = logging.getLogger(__name__)

# What the controller sends that the daemon acts on. Everything else from it (acks, chatter) stays here.
INPUT_CMDS = frozenset({"ptt", "key", "focus", "music"})    # music: Spotify mode's clicks, holds, dial
# Commands mirrored from the robot link. Heartbeats (no "cmd", a "total") and time sync are mirrored too.
MIRRORED_CMDS = frozenset({"agent", "listen"})
SILENT = {"cmd": "sound", "on": False}


def controller_serial(environ: Optional[Mapping[str, str]] = None) -> Optional[str]:
    env = os.environ if environ is None else environ
    raw = (env.get("EVE_CONTROLLER_SERIAL") or "").strip()
    return raw or None


def mirrored(obj: dict[str, Any]) -> bool:
    """Whether a message for the robot is one the controller shows too."""
    cmd = obj.get("cmd")
    if cmd is not None:
        return cmd in MIRRORED_CMDS
    return "total" in obj or "time" in obj


class Controller:
    """The controller board's own serial link: input in, a mirror of the robot's state out."""

    def __init__(self, serial_number: str, on_input: Callable[[dict[str, Any]], Awaitable[None]],
                 link_factory: Callable[..., Any] = EveSerial) -> None:
        self.serial_number = serial_number
        self.on_input = on_input
        self.link = link_factory(on_message=self._on_message, port=USB_SERIAL_PREFIX + serial_number)
        self.last: dict[str, dict[str, Any]] = {}      # the latest of each mirrored kind, replayed on connect
        self.link.on_boot = self._replay

    @property
    def connected(self) -> bool:
        return bool(self.link.connected)

    async def _on_message(self, obj: dict[str, Any]) -> None:
        if obj.get("cmd") in INPUT_CMDS:
            await self.on_input(obj)

    async def mirror(self, obj: dict[str, Any]) -> None:
        """A message the robot was sent: keep it for the next connect, and send it now if it is shown."""
        if not mirrored(obj):
            return
        kind = str(obj.get("cmd") or ("time" if "time" in obj else "heartbeat"))
        self.last[kind] = obj
        if self.connected:
            with contextlib.suppress(Exception):
                await self.link.send(obj)

    async def show(self, obj: dict[str, Any]) -> None:
        """A message for this board only (Spotify mode, music_mode.py). ``music_mode`` is kept and replayed on
        the next connect, so a board that reboots comes back green; ring levels and flashes are only sent."""
        if obj.get("cmd") == "music_mode":
            self.last["music_mode"] = obj
        if self.connected:
            with contextlib.suppress(Exception):
                await self.link.send(obj)

    async def send_audio(self, obj: dict[str, Any]) -> None:
        """eve's voice for the controller's speaker (desk_call.BoardSpeaker): sent only to this board."""
        if self.connected:
            await self.link.send(obj)

    async def _replay(self) -> None:
        for obj in [SILENT, *self.last.values()]:
            with contextlib.suppress(Exception):
                await self.link.send(obj)

    async def run(self) -> None:
        """The link's own reconnect loop, plus a replay of the current state each time it (re)connects."""
        log.info("controller: looking for the board with USB serial %s", self.serial_number)
        link_task = asyncio.create_task(self.link.run(), name="controller-link")
        try:
            while not link_task.done():
                await self.link.wait_connected()
                log.info("controller: connected")
                await self._replay()
                while self.connected and not link_task.done():
                    await asyncio.sleep(1.0)
                if not link_task.done():
                    log.info("controller: disconnected")
                    await asyncio.sleep(0.5)           # the link clears its connected flag as it closes
        finally:
            link_task.cancel()
            await asyncio.gather(link_task, return_exceptions=True)

    async def stop(self) -> None:
        with contextlib.suppress(Exception):
            await self.link.stop()
