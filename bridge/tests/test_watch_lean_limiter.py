"""Replay tests for the Lean model verification/Eve/WatchLimiter.lean (RateLimiter in watch.py).

Each test is one counterexample trace the model found, driven against the real code. The properties that held
(the bucket never over-issues, a take after ready_in == 0 never goes into debt, the penalty bounds) are pinned by
the passing tests at the end, so a later edit that breaks one of them is caught here too.
"""

from __future__ import annotations

import asyncio
import random
from pathlib import Path
from typing import Any

import pytest

from eve_bridge.watch import FetchError, RateLimiter, WatchConfig, Watcher

YAHOO = "finance.yahoo.com"


class Clock:
    def __init__(self, t: float = 1_800_000_000.0) -> None:
        self.t = t

    def __call__(self) -> float:
        return self.t


def lim(clock: Clock, **kw: Any) -> RateLimiter:
    opts: dict[str, Any] = dict(rate_per_min=6, burst=2, host_gap_secs=20, backoff_base=60, backoff_max=3600,
                                model_calls_per_day=2, clock=clock, day=lambda t: str(int(t // 86400)))
    opts.update(kw)
    return RateLimiter(**opts)


# ---- bug 1: a second penalty shortens a backoff that is still in force --------------------------------------

def test_a_second_penalty_never_shortens_a_backoff_in_force() -> None:
    """Lean: current_violates_backoff. The host said Retry-After 3000; a request let through before that answer
    comes back refused with no Retry-After (strike 2: 120 s). The host is then held 120 s, not the 2990 s left of
    what the server asked for."""
    clock = Clock()
    rl = lim(clock)
    assert rl.penalize("h", 3000) == 3000
    clock.t += 10
    rl.penalize("h")
    assert rl.backed_off("h") >= 2990


def race(tmp_path: Path) -> tuple[RateLimiter, list[float]]:
    """watch_add holds the check lock on a slow Yahoo read; meanwhile the loop's tick lets a second Yahoo watch
    through the limiter (the 20 s host gap has passed, no backoff yet), takes its token and waits on the lock.
    Yahoo answers the first read 429 Retry-After 3000; the lock passes to the tick, whose check was let through
    before that answer. Every later Yahoo read is refused with no Retry-After. Returns the limiter, and what
    ``backed_off(yahoo)`` was at each moment a Yahoo read went out."""
    clock = Clock()
    limiter = RateLimiter(clock=clock, day=lambda t: str(int(t // 86400)))          # the shipped defaults
    first_read = asyncio.Event()
    sent: list[float] = []

    async def no_sleep(_s: float) -> None:
        return None

    w = Watcher(WatchConfig(enabled=True, path=tmp_path / "w.json"), notify=None,
                fetch=lambda url, **kw: (200, ""), ask=lambda body: {}, clock=clock, sleep=no_sleep,
                rng=random.Random(0), limiter=limiter, offload=False, resolve=lambda host: True)

    async def call(fn: Any, *args: Any, **kw: Any) -> Any:
        sent.append(limiter.backed_off(YAHOO))
        if len(sent) == 1:
            await first_read.wait()                                     # the slow first read
            clock.t += 10
            raise FetchError("the site answered HTTP 429", 429, 3000.0)
        raise FetchError("the site answered HTTP 429", 429, None)

    w._call = call  # type: ignore[method-assign]

    async def scene() -> None:
        other, why = w.validate({"kind": "quote", "target": "MSFT", "label": "Microsoft", "condition": "change",
                                 "value": None, "text": None, "field": None, "every_minutes": None})
        assert other is not None, why
        w.watches.append(other)
        other.next_at = clock.t + 25
        adding = asyncio.create_task(w.add({"kind": "quote", "target": "AAPL", "label": "Apple",
                                            "condition": "change", "value": None, "text": None, "field": None,
                                            "every_minutes": None}))
        await asyncio.sleep(0)                                          # add takes its token and holds the lock
        assert len(sent) == 1
        clock.t += 25                                                   # past the host gap; MSFT is due
        ticking = asyncio.create_task(w.tick())
        await asyncio.sleep(0)                                          # tick: ready_in == 0, take, wait on the lock
        first_read.set()
        await adding
        await ticking

    asyncio.run(scene())
    return limiter, sent


def test_a_stale_gate_cannot_shorten_the_hosts_backoff(tmp_path: Path) -> None:
    """Bug 1 through the Watcher: whatever the tick's stale check does, Yahoo's 3000 s must still stand."""
    limiter, _ = race(tmp_path)
    assert limiter.backed_off(YAHOO) >= 2990


# ---- bug 3: a check let through before a refusal reads inside the hold ---------------------------------------

def test_no_read_goes_to_a_host_inside_its_hold(tmp_path: Path) -> None:
    """Lean: current_violates_gate. The tick's MSFT check passed ready_in before Yahoo's 429 landed; under the
    lock it must see the hold and push the watch back, not read."""
    _, sent = race(tmp_path)
    assert sent[0] == 0                              # add's own read went before any hold
    assert [x for x in sent[1:] if x > 0] == []      # no read while the host was held


# ---- bug 2: the daily model cap resets when the day goes back ------------------------------------------------

def test_a_day_that_used_its_cap_stays_used_when_the_clock_comes_back() -> None:
    """Lean: current_violates_model_cap. The cap is spent late on day D; the clock (or the Mac's time zone) steps
    across midnight to D+1 and back to D. Day D had its model calls; it must not get a second allowance."""
    clock = Clock(86400 * 100 + 86390)
    rl = lim(clock)
    rl.model_used()
    rl.model_used()
    assert not rl.model_ok()
    clock.t += 20                          # D+1: a new day, a fresh cap
    assert rl.model_ok()
    clock.t -= 20                          # back on D
    assert not rl.model_ok()


# ---- the properties that held: pinned against the real code --------------------------------------------------

def test_back_and_forth_clock_mints_nothing_a_monotone_clock_would_not() -> None:
    """Lean: cur_bucket_matches_reference. After any wiggle the bucket equals one refilled on the max-so-far clock."""
    clock = Clock()
    rl = lim(clock, burst=3, host_gap_secs=0)
    for h in "abc":
        rl.take(h)
    for step in (+5, -30, +40, -25, +7, -100, +103):
        clock.t += step
        rl.ready_in("x")
    # the furthest the clock ever got is +15 over the takes: 15 s at 6/min is 1.5 tokens, never more
    assert rl._tokens == pytest.approx(1.5)


def test_ready_then_take_never_goes_into_debt() -> None:
    clock = Clock()
    rl = lim(clock, burst=2, host_gap_secs=0)
    for step in (0, 0, 0, 3, 4, 10, 1, 9, 30):
        clock.t += step
        if rl.ready_in("a") == 0:
            rl.take("a")
            assert rl._tokens >= 0


def test_penalty_bounds_hold() -> None:
    clock = Clock()
    rl = lim(clock, backoff_max=600)
    for ra in (None, 5, 999, None, 0, 30, None, 10_000):
        secs = rl.penalize("h", ra)
        assert secs <= 600 and secs >= min(ra or 0, 600)
    rl.clear("h")
    assert rl.backed_off("h") == 0 and rl.penalize("h") == 60
