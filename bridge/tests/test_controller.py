"""controller.py: the Voice PE next to the StackChan. Input from it reaches the daemon; the robot's state is
mirrored to it; its acks and chatter stay out of the robot's link. A fake link stands in for the serial port."""
from __future__ import annotations

import asyncio
import functools
from typing import Any

from eve_bridge import controller


def run(test):
    @functools.wraps(test)
    def sync(*args: Any, **kwargs: Any) -> None:
        asyncio.run(test(*args, **kwargs))
    return sync


class Link:
    def __init__(self, on_message, port: str) -> None:
        self.on_message, self.port = on_message, port
        self.sent: list[dict[str, Any]] = []
        self.connected = False
        self.on_boot = None
        self._evt = asyncio.Event()

    async def send(self, obj: dict[str, Any]) -> bool:
        self.sent.append(obj)
        return True

    async def wait_connected(self) -> None:
        await self._evt.wait()

    def plug(self, on: bool) -> None:
        self.connected = on
        (self._evt.set if on else self._evt.clear)()

    async def run(self) -> None:
        await asyncio.Event().wait()

    async def stop(self) -> None:
        pass


def make() -> tuple[controller.Controller, list[dict[str, Any]]]:
    got: list[dict[str, Any]] = []

    async def on_input(obj: dict[str, Any]) -> None:
        got.append(obj)

    return controller.Controller("0A:00:00:00:00:02", on_input, link_factory=Link), got


HEARTBEAT = {"total": 2, "running": 1, "waiting": 1, "msg": "approve: Bash"}


def test_the_controller_is_named_by_its_usb_serial() -> None:
    ctl, _ = make()
    assert ctl.link.port == "usbsn:0A:00:00:00:00:02"
    assert controller.controller_serial({"EVE_CONTROLLER_SERIAL": " 0A:00:00:00:00:02 "}) == "0A:00:00:00:00:02"
    assert controller.controller_serial({}) is None


def test_only_what_the_controller_can_show_is_mirrored() -> None:
    assert controller.mirrored(HEARTBEAT)
    assert controller.mirrored({"time": [1, 2]})
    for cmd in ("agent", "listen"):
        assert controller.mirrored({"cmd": cmd})
    for cmd in ("sound", "status", "look", "cam", "caption", "move", "expression", "char_begin", "unpair"):
        assert not controller.mirrored({"cmd": cmd})


@run
async def test_input_reaches_the_daemon_and_acks_do_not() -> None:
    ctl, got = make()
    for obj in ({"cmd": "ptt", "on": True}, {"cmd": "key", "name": "enter"}, {"cmd": "focus"},
                {"ack": "sound", "ok": True, "n": 0}, {"ack": "status", "ok": True}, {"cmd": "explore"}):
        await ctl.link.on_message(obj)
    assert got == [{"cmd": "ptt", "on": True}, {"cmd": "key", "name": "enter"}, {"cmd": "focus"}]


@run
async def test_state_is_sent_while_connected_and_replayed_on_connect() -> None:
    ctl, _ = make()
    await ctl.mirror(HEARTBEAT)                           # not connected: kept, not sent
    await ctl.mirror({"cmd": "agent", "state": "thinking"})
    await ctl.mirror({"cmd": "look", "yaw": 10})          # not shown: neither kept nor sent
    assert ctl.link.sent == []

    task = asyncio.create_task(ctl.run())
    ctl.link.plug(True)
    await asyncio.sleep(0.05)
    assert ctl.link.sent == [controller.SILENT, HEARTBEAT, {"cmd": "agent", "state": "thinking"}]

    await ctl.mirror({"cmd": "agent", "state": "speaking"})
    assert ctl.link.sent[-1] == {"cmd": "agent", "state": "speaking"}

    # a board reboot on an open link replays the latest of each kind
    ctl.link.sent.clear()
    await ctl.link.on_boot()
    assert ctl.link.sent == [controller.SILENT, HEARTBEAT, {"cmd": "agent", "state": "speaking"}]
    task.cancel()
    await asyncio.gather(task, return_exceptions=True)


@run
async def test_the_beeps_are_the_robots_alone() -> None:
    """The owner turning sound on reaches the robot, never the controller; the controller is silenced on connect."""
    ctl, _ = make()
    task = asyncio.create_task(ctl.run())
    ctl.link.plug(True)
    await asyncio.sleep(0.05)
    await ctl.mirror({"cmd": "sound", "on": True})
    assert {"cmd": "sound", "on": True} not in ctl.link.sent
    assert ctl.link.sent[0] == {"cmd": "sound", "on": False}
    task.cancel()
    await asyncio.gather(task, return_exceptions=True)
