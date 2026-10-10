"""codex_warm.py: one Codex agent started ahead of time; never a stale one handed out."""

from __future__ import annotations

import asyncio
from typing import Any

from eve_bridge import codex_warm
from eve_bridge.codex_warm import WarmCodex


class Fake:
    made: list["Fake"] = []

    def __init__(self, *, on_event: Any, ask_user: Any, fail: bool = False) -> None:
        self.on_event, self.ask_user = on_event, ask_user
        self.warmed = self.discarded = False
        self.alive = True
        self.fail = fail
        Fake.made.append(self)

    async def prewarm(self) -> None:
        if self.fail:
            raise OSError("no codex")
        self.warmed = True

    def warm(self, max_age: float) -> bool:
        return self.warmed and self.alive

    async def discard(self) -> None:
        self.discarded = True


def test_take_hands_the_warm_agent_this_tasks_callbacks() -> None:
    async def go() -> None:
        Fake.made = []
        pool = WarmCodex(Fake)
        pool.kick()
        await asyncio.gather(*pool._jobs)
        cb = object()
        agent = pool.take(cb, cb)
        assert agent is Fake.made[0] and agent.on_event is cb and agent.ask_user is cb
        cold = pool.take(cb, cb)                          # none ready now: a new cold agent
        assert cold is not agent and not cold.warmed and len(Fake.made) == 2
    asyncio.run(go())


def test_a_dead_warm_agent_is_discarded_not_handed_out() -> None:
    async def go() -> None:
        Fake.made = []
        pool = WarmCodex(Fake)
        pool.kick()
        await asyncio.gather(*pool._jobs)
        Fake.made[0].alive = False
        agent = pool.take(None, None)
        await asyncio.gather(*pool._jobs)
        assert agent is not Fake.made[0] and Fake.made[0].discarded
    asyncio.run(go())


def test_kick_is_single_and_disabled_does_nothing() -> None:
    async def go() -> None:
        Fake.made = []
        pool = WarmCodex(Fake)
        pool.kick()
        pool.kick()
        await asyncio.gather(*pool._jobs)
        pool.kick()                                        # one is ready: nothing more
        await asyncio.gather(*pool._jobs)
        assert len(Fake.made) == 1
        off = WarmCodex(Fake, enabled=False)
        off.kick()
        assert len(Fake.made) == 1
    asyncio.run(go())


def test_failed_warm_up_leaves_tasks_cold() -> None:
    async def go() -> None:
        pool = WarmCodex(lambda **kw: Fake(fail=True, **kw))
        pool.kick()
        await asyncio.gather(*pool._jobs)
        assert pool._ready is None and pool.take(None, None) is not None
    asyncio.run(go())


def test_close_discards_the_ready_agent() -> None:
    async def go() -> None:
        Fake.made = []
        pool = WarmCodex(Fake)
        pool.kick()
        await asyncio.gather(*pool._jobs)
        await pool.close()
        assert Fake.made[0].discarded and pool._ready is None
    asyncio.run(go())


def test_configured() -> None:
    assert codex_warm.configured({}) == (True, codex_warm.DEFAULT_MAX_AGE_SECS)
    assert codex_warm.configured({"EVE_CODEX_PREWARM": "0", "EVE_CODEX_WARM_SECS": "5"}) == (False, 60.0)
