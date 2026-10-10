"""The daemon stops a running desktop task with a readable reason on restart and on a
touch-hush, and tells the robot (bench 2026-09-06: a task died silently)."""

from __future__ import annotations

import asyncio
from types import MethodType, SimpleNamespace

from eve_bridge.daemon import Daemon


class _Ble:
    def __init__(self) -> None:
        self.connected = True
        self.sent: list[dict] = []

    async def send(self, obj: dict) -> bool:
        self.sent.append(obj)
        return True


class _Agent:
    def __init__(self, running: bool = True) -> None:
        self.running = running
        self.reason = None

    def cancel(self, reason: str = "") -> None:
        self.reason = reason
        self.running = False


def _daemon(agent) -> SimpleNamespace:
    d = SimpleNamespace(ble=_Ble(), _active_agent=agent)
    d._cancel_active_task = MethodType(Daemon._cancel_active_task, d)
    return d


def test_cancel_active_task_cancels_and_captions() -> None:
    agent = _Agent()
    d = _daemon(agent)
    asyncio.run(d._cancel_active_task("the daemon is restarting"))
    assert agent.reason == "the daemon is restarting"
    assert d.ble.sent == [{"cmd": "caption", "lines": ["stopped: the daem"], "page": 0, "of": 1, "hold_ms": 4000,
                           "final": True, "chirp": False}]


def test_cancel_active_task_is_a_noop_without_a_running_task() -> None:
    d = _daemon(_Agent(running=False))
    asyncio.run(d._cancel_active_task("x"))
    assert d.ble.sent == []
    d2 = _daemon(None)
    asyncio.run(d2._cancel_active_task("x"))
    assert d2.ble.sent == []
