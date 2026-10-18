"""Keep one Codex computer agent warm, so a hard task's handoff starts the turn at once.

A cold ``CodexComputerAgent.run`` spends 4.8-7.0 s (measured 2026-09-23) before the goal is used at
all: the app-server process, ``initialize``, an ephemeral thread, and the check that Computer Use's
``cua_repl`` tool is there. None of that depends on the goal, so ``WarmCodex`` does it ahead of time
(``CodexComputerAgent.prewarm``) and hands the ready agent to the next task that needs Codex.

* One warm agent at most, and it is single-use: the task closes it, as a cold one is closed.
* The next one is warmed after a task ends (``kick``), not while it runs: two app-servers never
  share the desktop during a task.
* A warm agent older than ``EVE_CODEX_WARM_SECS`` (default 900), or whose app-server died, is
  thrown away and replaced by the refresh loop; ``take`` never hands out a stale one — the task then
  starts cold, exactly as before this module.
* ``EVE_CODEX_PREWARM=0`` turns it off. A warm-up that fails is logged and retried on the next
  refresh; it never touches a task.
"""

from __future__ import annotations

import asyncio
import logging
import os
from typing import Any, Callable, Optional

log = logging.getLogger(__name__)

DEFAULT_MAX_AGE_SECS = 900.0
REFRESH_SECS = 60.0


def configured(environ: Any = None) -> tuple[bool, float]:
    env = os.environ if environ is None else environ
    on = str(env.get("EVE_CODEX_PREWARM", "1")).strip().lower() not in ("0", "false", "no", "off")
    try:
        age = float(env.get("EVE_CODEX_WARM_SECS", "") or DEFAULT_MAX_AGE_SECS)
    except ValueError:
        age = DEFAULT_MAX_AGE_SECS
    return on, max(60.0, age)


async def _nobody(question: str) -> str:
    return ""                     # a warm agent has no task; it asks nothing until take() gives it one


class WarmCodex:
    def __init__(self, factory: Callable[..., Any], *, enabled: bool = True,
                 max_age: float = DEFAULT_MAX_AGE_SECS) -> None:
        self._factory, self.enabled, self.max_age = factory, enabled, max_age
        self._ready: Any = None
        self._warming: Optional[asyncio.Task] = None
        self._jobs: set[asyncio.Task] = set()
        # take() handed an agent to a task and kick() has not said it ended: the refresh loop warms nothing,
        # or a second app-server would share the desktop with the task
        self._in_use = False

    def _spawn(self, coro: Any) -> asyncio.Task:
        task = asyncio.ensure_future(coro)
        self._jobs.add(task)
        task.add_done_callback(self._jobs.discard)
        return task

    def kick(self) -> None:
        """A task ended (app_reflex's on_done), or the daemon starts: warm one now, unless one is ready or
        warming. Safe to call from any callback on the loop."""
        self._in_use = False
        self._kick()

    def _kick(self) -> None:
        if (not self.enabled or self._in_use or self._ready is not None
                or (self._warming is not None and not self._warming.done())):
            return
        self._warming = self._spawn(self._warm())

    async def _warm(self) -> None:
        agent = self._factory(on_event=lambda ev: None, ask_user=_nobody)
        try:
            await agent.prewarm()
        except asyncio.CancelledError:
            await agent.discard()
            raise
        except Exception as e:  # noqa: BLE001 — Codex missing or down: tasks start cold, as before
            log.warning("codex-warm: warm-up failed (%s); tasks start cold until the next refresh", type(e).__name__)
            return
        self._ready = agent
        log.info("codex-warm: an agent is ready")

    def take(self, on_event: Callable[..., None], ask_user: Callable[..., Any]) -> Any:
        """The warm agent, given this task's callbacks; or a new cold one when none is ready. Nothing is warmed
        again until kick() says the task ended, and a warm-up still in flight is called off."""
        self._in_use = True
        if self._warming is not None and not self._warming.done():
            self._warming.cancel()                     # _warm discards what it started
        agent, self._ready = self._ready, None
        if agent is not None and agent.warm(self.max_age):
            agent.on_event, agent.ask_user = on_event, ask_user
            return agent
        if agent is not None:
            self._spawn(agent.discard())
        return self._factory(on_event=on_event, ask_user=ask_user)

    async def refresh_loop(self, every: float = REFRESH_SECS) -> None:
        """Warm one at start, and replace it whenever it goes stale or its app-server dies."""
        if not self.enabled:
            return
        try:
            while True:
                ready = self._ready
                if ready is not None and not ready.warm(self.max_age):
                    self._ready = None
                    await ready.discard()
                self._kick()                           # never while a task holds the one it took
                await asyncio.sleep(every)
        finally:
            await self.close()

    async def close(self) -> None:
        if self._warming is not None:
            self._warming.cancel()
        ready, self._ready = self._ready, None
        if ready is not None:
            await ready.discard()
        await asyncio.gather(*list(self._jobs), return_exceptions=True)
