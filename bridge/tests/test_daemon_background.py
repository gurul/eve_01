"""Fire-and-forget daemon work is held in `_background` (review D1): the loop keeps only a weak
reference to a task, so an unheld one can be collected mid-run, and shutdown cancels it rather
than draining it."""

from __future__ import annotations

import asyncio
from types import MethodType, SimpleNamespace

from eve_bridge.daemon import Daemon


class _State:
    sessions: dict = {}

    def pulse_completed(self, duration_secs: float) -> None:
        self.pulsed = duration_secs


def _stub() -> SimpleNamespace:
    d = SimpleNamespace(_background=set(), _pending_turn_ends={}, state=_State(), _telegram=None)
    d.side_effects = asyncio.Event()

    async def deferred(session_id: str, delay: float) -> None:
        return None

    async def side_effects(celebrate_secs: float) -> None:
        await d.side_effects.wait()

    d._deferred_turn_end = deferred
    d._turn_end_side_effects = side_effects
    d._ipc_turn_end = MethodType(Daemon._ipc_turn_end, d)
    return d


def test_turn_end_side_effects_task_is_held_in_background() -> None:
    async def run() -> None:
        d = _stub()
        assert await d._ipc_turn_end({"session_id": "s1"}) == {"ok": True}
        assert len(d._background) == 1          # the side-effects task, held until it finishes
        d.side_effects.set()
        await asyncio.gather(*list(d._background))
        await asyncio.sleep(0)
        assert d._background == set()           # discarded once done

    asyncio.run(run())


def test_drain_background_waits_for_held_work() -> None:
    async def run() -> None:
        d = _stub()
        await d._ipc_turn_end({"session_id": "s1"})
        task = next(iter(d._background))
        asyncio.get_running_loop().call_later(0.05, d.side_effects.set)
        await Daemon._drain_background(d)
        assert task.done() and not task.cancelled()

    asyncio.run(run())
