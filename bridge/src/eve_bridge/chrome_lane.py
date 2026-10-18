"""The fast path into the owner's logged-in Chrome: plan once, execute in eve's own tab, Codex as the floor.

Owner, 2026-09-23: "i want eve to be able to control logged in browser, that's the most important".
Codex already drives the owner's Chrome; this is the faster road to the same browser. The daemon holds ONE
browser_lane.BrowserLane in attach mode for its whole life — Chrome asks "Allow remote debugging?" per
connection, so one connection means one click per Chrome session, not one per task. For each task:

1. A fresh ComputerAgent (the planner, gpt-6-astra) runs ``run_in_browser``: one plan against the page in
   eve's tab, executed step by step with Jev grounding each click, a sensitive step (buy, send, delete…)
   stopping for the owner's yes (consent.py), and a code check of each step's expected result.
2. Finished: that is the answer. Stopped by the owner's no: that is the answer.
3. Part done: the rest goes to Codex with a note of what was already done, so nothing is repeated.
   Nothing done (no plan, the lane unreachable, the owner not clicking Allow): Codex takes the whole task,
   exactly as before this module. "Nothing done" has a clock: when no plan is executing and no step has been
   reported NOTHING_DONE_BUDGET_SECS after the start, the lane is stopped and Codex takes the task then. A
   plan that has started runs to its end.

``ChromeLaneAgent`` keeps the run/steer/cancel/status contract every door already uses, so it drops in behind
app_reflex.ReflexFirstAgent as its second body (make_auto) with no change to the voice or Telegram doors.
"""

from __future__ import annotations

import asyncio
import logging
import time
from typing import Any, Awaitable, Callable, Optional

from .agent_contract import AgentEvent

log = logging.getLogger(__name__)

# How long the lane may go with nothing done (no plan executing, no step reported) before Codex takes the task.
# Production, 2026-09-23/24: once connected, the lane reached a plan's first step (or gave up) 4-6 s later; the
# runs that did nothing still spent 11.3 s and 25.6 s before handing on (19 s of it one profile lookup, now
# budgeted at 3 s), and one waited 36 s on Chrome's Allow until the owner pressed Stop. 15 s is about twice
# the slowest healthy path; a plan that has started is never cut off.
NOTHING_DONE_BUDGET_SECS = 15.0


class ChromeLaneAgent:
    """Plan once in the owner's Chrome (``make_planner().run_in_browser``), Codex (``make_fallback``) for the rest."""

    provider = "chrome-lane"

    def __init__(self, make_planner: Callable[[Callable[[AgentEvent], None], Callable[[str], Awaitable[str]]], Any],
                 make_fallback: Callable[[], Any], on_event: Callable[[AgentEvent], None],
                 ask_user: Callable[[str], Awaitable[str]],
                 prepare: Optional[Callable[[str], Awaitable[Any]]] = None,
                 lane_screenshot: Optional[Callable[[], Awaitable[Optional[bytes]]]] = None,
                 idle_budget_secs: Optional[float] = None,
                 before_fallback: Optional[Callable[[], Awaitable[Any]]] = None) -> None:
        self._make_planner, self._make_fallback = make_planner, make_fallback
        self._idle_budget = NOTHING_DONE_BUDGET_SECS if idle_budget_secs is None else idle_budget_secs
        self._progressed = self._timed_out = False
        self._prepare = prepare                       # connect (the owner's Allow) and pick the Chrome profile
        self._lane_screenshot = lane_screenshot       # eve's tab in the owner's Chrome, as JPEG bytes
        self._before_fallback = before_fallback       # make sure the owner's Chrome is up for Codex's extension
        self.on_event, self.ask_user = on_event, ask_user
        self._current: Any = None                    # the planner, then (maybe) Codex
        self._cancel_reason: Optional[str] = None
        self._preparing: Optional[asyncio.Future] = None  # connecting / waiting on the owner's Allow: cancellable
        self.goal = self.final = ""
        self.handed_on = False                        # Codex finished what the lane started (or did it all)
        self.browser_used = True

    @property
    def running(self) -> bool:
        return bool(getattr(self._current, "running", False))

    def status(self) -> dict[str, Any]:
        inner = self._current.status() if hasattr(self._current, "status") else {}
        return {**inner, "provider": self.provider, "handed_on": self.handed_on}

    def steer(self, text: str) -> bool:
        return bool(self._current is not None and self._current.steer(text))

    def cancel(self, reason: str = "") -> None:
        self._cancel_reason = reason
        if self._preparing is not None and not self._preparing.done():
            self._preparing.cancel()                   # "stop" while waiting for Chrome's Allow is immediate
        if self._current is not None:
            self._current.cancel(reason=reason)

    async def browser_shot(self) -> Optional[tuple[bytes, str, str]]:
        """(image bytes, suffix, caption) of the browser this task used — never the desktop: eve's tab in the
        owner's Chrome (the page itself, full size), or Codex's captured tab once Codex took over. None when
        there is none (the caller then says so or falls back)."""
        if self.handed_on:
            shot = getattr(self._current, "browser_screenshot", None)
            if shot is not None:
                return shot.data, shot.suffix, "The browser tab Codex worked in (last captured view)"
            return None
        if self._lane_screenshot is not None:
            data = await self._lane_screenshot()
            if data:
                return data, ".jpg", "Your Chrome: eve's tab"
        return None

    def __getattr__(self, name: str) -> Any:          # browser_screenshot, ui_evidence … from whoever ran last
        if name.startswith("_") or self.__dict__.get("_current") is None:
            raise AttributeError(name)
        return getattr(self._current, name)

    async def run(self, goal: str) -> str:
        self.goal = goal
        self.on_event(AgentEvent("started", goal))
        t0 = time.perf_counter()
        planner = self._current = self._make_planner(self._forward, self.ask_user)
        self._progressed, self._timed_out = False, False
        lane = getattr(planner, "browser", None)
        plans_before = getattr(lane, "plans_started", 0)
        attempt = asyncio.ensure_future(self._attempt(planner, goal))
        try:
            await asyncio.wait({attempt}, timeout=self._idle_budget)
            if not attempt.done() and not self._progressed and getattr(lane, "plans_started", 0) == plans_before:
                # Nothing done yet (no plan executing, not a step reported): the lane is stuck connecting,
                # waiting on Chrome's Allow, or planning slowly. Codex takes it now rather than later.
                self._timed_out = True
                log.info("chrome-lane: nothing done within %.0f s; Codex takes it", self._idle_budget)
                if self._preparing is not None and not self._preparing.done():
                    self._preparing.cancel()
                planner.cancel(reason=f"no progress in {self._idle_budget:.0f} s")
            answer, note = await attempt
        except asyncio.CancelledError:
            attempt.cancel()
            raise
        if self._timed_out:
            answer, note = "", ""                         # "Stopped: no progress…" is ours, not the owner's answer
        if self._cancel_reason is not None:
            self.final = answer or "Stopped."
            self.on_event(AgentEvent("cancelled", self.final))
            return self.final
        if answer:
            log.info("chrome-lane: done in the owner's Chrome in %.1f s", time.perf_counter() - t0)
            self.final = answer
            self.on_event(AgentEvent("final", answer))
            return answer
        self.handed_on = True
        log.info("chrome-lane: %s after %.1f s; Codex takes it", "part done" if note else "nothing done",
                 time.perf_counter() - t0)
        if note:
            self.on_event(AgentEvent("progress", "Part of it is done in your Chrome; Codex is finishing it."))
        if self._before_fallback is not None:
            try:
                await self._before_fallback()
            except Exception as e:  # noqa: BLE001 — Codex still runs and says what it cannot reach
                log.warning("chrome-lane: getting the owner's Chrome ready failed (%s)", e)
        if self._cancel_reason is not None:
            # A Stop while the owner's Chrome was getting ready reached only the finished planner: Codex
            # must not start the whole task after it.
            self.final = "Stopped."
            self.on_event(AgentEvent("cancelled", self.final))
            return self.final
        self._current = self._make_fallback()
        self.final = await self._current.run(goal + note)
        return self.final

    async def _attempt(self, planner: Any, goal: str) -> tuple[str, str]:
        """Connect, pick the profile, plan and execute: (answer, note), or ("", "") when the lane cannot."""
        try:
            if self._prepare is not None:
                self._preparing = asyncio.ensure_future(self._prepare(goal))
                try:
                    await self._preparing             # raises when Chrome cannot be reached: Codex takes it
                except asyncio.CancelledError:
                    if self._timed_out:
                        return "", ""                 # our own budget ran out while connecting: Codex takes it
                    if self._cancel_reason is None:
                        raise                         # the whole task was cancelled from outside: let it go
                    return "Stopped.", ""
            if self._timed_out:
                return "", ""                         # the budget ran out as the connection came up
            return await planner.run_in_browser(goal)
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001 — the lane never costs the task: Codex takes it
            log.warning("chrome-lane: the lane failed (%s: %s); Codex takes the task", type(e).__name__,
                        str(e).splitlines()[0][:300] if str(e) else "")
            return "", ""

    def _forward(self, ev: AgentEvent) -> None:
        # The planner's own started/final are this agent's to say; its progress and questions pass through.
        if ev.kind not in ("started", "final"):
            self._progressed = True                   # a step done, or a question: the plan is under way
            self.on_event(ev)
