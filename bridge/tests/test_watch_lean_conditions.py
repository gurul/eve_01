"""Replays for verification/Eve/WatchConditions.lean: evaluate()'s conditions and the state they keep.

The first three tests fail on the code the model calls current (a baseline of 0 raises ZeroDivisionError, and a
tick that hits it stops every other watch); the fourth fails on float rounding at an exact percentage. The rest
pin behaviour the model proves or relies on (the restart round trip, the None-first readings)."""

from __future__ import annotations

import asyncio
import json
import random
from pathlib import Path
from typing import Any, Optional

from eve_bridge import watch
from eve_bridge.watch import RateLimiter, Reading, Watch, WatchConfig, Watcher


def run(w: Watch, values: list[Optional[float]]) -> list[Optional[str]]:
    return [watch.evaluate(w, Reading(value=v)) for v in values]


def pct_watch(condition: str, pct: float) -> Watch:
    return Watch(id="w1", kind="page", target="https://shop.example/p", label="Thing", condition=condition, value=pct)


def restart(w: Watch) -> Watch:
    back = Watch.from_json(json.loads(json.dumps(w.to_json())))
    assert back is not None
    return back


# ---- the bug: a baseline of 0 (Lean: current_violates, current_violates_after_a_drop_to_zero) ---------

def test_a_first_price_of_zero_does_not_raise_on_the_next_reading() -> None:
    w = pct_watch("rise_pct", 10)
    alerts = run(w, [0.0, 5.0])            # raises ZeroDivisionError on the current code
    assert alerts == [None, None] and w.baseline == 5.0
    assert run(w, [6.0])[0] is not None    # the watch keeps answering: +20% from the new baseline alerts


def test_a_drop_to_zero_alerts_then_the_watch_keeps_working_across_a_restart() -> None:
    w = pct_watch("drop_pct", 10)
    alerts = run(w, [2.0, 0.0])
    assert alerts[0] is None and alerts[1] is not None and w.baseline == 0.0
    w = restart(w)                         # the 0 baseline was saved with the check that set it
    assert run(w, [1.0]) == [None]         # raises ZeroDivisionError on the current code
    assert w.baseline == 1.0


class Clock:
    def __init__(self, t: float = 1_800_000_000.0) -> None:
        self.t = t

    def __call__(self) -> float:
        return self.t


def quote_body(price: float, symbol: str) -> str:
    return json.dumps({"chart": {"result": [{"meta": {"symbol": symbol, "currency": "USD",
                                                       "regularMarketPrice": price, "shortName": symbol}}]}})


def test_a_zero_baseline_does_not_stop_the_other_watches(tmp_path: Path) -> None:
    clock = Clock()
    sent: list[str] = []

    async def notify(text: str) -> None:
        sent.append(text)

    async def no_sleep(_s: float) -> None:
        return None

    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        return 200, (quote_body(5, "FREE") if "FREE" in url else quote_body(290, "AAPL"))

    lim = RateLimiter(rate_per_min=600, burst=50, host_gap_secs=0, backoff_base=60, backoff_max=600,
                      model_calls_per_day=50, clock=clock, day=lambda t: str(int(t // 86400)))
    wr = Watcher(WatchConfig(enabled=True, path=tmp_path / "w.json"), notify=notify, fetch=fetch,
                 ask=lambda body: {}, clock=clock, sleep=no_sleep, rng=random.Random(0), limiter=lim,
                 offload=False, resolve=lambda host: True)
    # a rise watch whose first price was 0, due first; an ordinary watch due after it
    zero = Watch(id="w1", kind="quote", target="FREE", label="Free", condition="rise_pct", value=10,
                 checks=1, last_value=0.0, baseline=0.0, every_secs=900, next_at=clock.t - 100)
    apple = Watch(id="w2", kind="quote", target="AAPL", label="Apple", condition="below", value=300,
                  checks=1, last_value=310.0, armed=True, every_secs=900, next_at=clock.t - 50)
    wr.watches = [zero, apple]
    asyncio.run(wr.tick())                 # raises ZeroDivisionError on the current code
    assert apple.checks == 2 and sent == ["Apple dropped to $290 (was $310), past your $300 mark."]
    assert zero.next_at > clock.t          # the zero watch was read and rescheduled, not left first in line


# ---- float rounding at the boundary (not in the Lean model, which compares exactly) --------------

def test_an_exact_percentage_move_alerts() -> None:
    drop = pct_watch("drop_pct", 10)
    assert run(drop, [3.0, 2.7])[1] is not None      # (3.0 - 2.7) / 3.0 * 100 == 9.999999999999996


# ---- pinned behaviour the model proves or relies on ----------------------------------------------

def test_a_restart_keeps_every_field_evaluate_reads() -> None:
    # every_secs as validate() sets it: the store clamps a loaded interval to the kind's floor (a hand-edited 0
    # would check on every tick), so a watch that never came from validate() is not what a restart reloads
    w = Watch(id="w1", kind="page", target="https://shop.example/p", label="Thing", condition="below", value=300,
              checks=3, last_value=310.5, last_available=False, last_hit=True, baseline=299.25, armed=False,
              every_secs=1800)
    back = restart(w)
    assert (back.checks, back.armed, back.baseline, back.last_value, back.last_available, back.last_hit) == \
        (3, False, 299.25, 310.5, False, True)
    assert back == w


def test_an_unknown_first_reading_then_one_that_holds_alerts_once() -> None:
    # Lean: unknown_start_alerts. Intended: the owner was told "no reading yet", so this is news.
    w = Watch(id="w1", kind="page", target="https://shop.example/p", label="Thing", condition="below", value=300)
    alerts = run(w, [None, 250.0, 240.0, None, 230.0])
    assert [a is not None for a in alerts] == [False, True, False, False, False]
    avail = Watch(id="w2", kind="search", target="tickets", label="Tour", condition="available")
    got = [watch.evaluate(avail, Reading(available=a)) for a in (None, True, None, True, False, True)]
    assert [a is not None for a in got] == [False, True, False, False, False, True]


def test_a_priceless_first_reading_leaves_the_baseline_to_the_first_price() -> None:
    # Lean: none_first_sets_baseline_later.
    w = pct_watch("drop_pct", 10)
    assert run(w, [None, 100.0]) == [None, None] and w.baseline == 100.0
    assert run(w, [90.0])[0] is not None and w.baseline == 90.0


def test_change_compares_against_the_last_known_value() -> None:
    w = Watch(id="w1", kind="page", target="https://shop.example/p", label="Thing", condition="change")
    got = [watch.evaluate(w, Reading(value=v, available=a))
           for v, a in ((None, None), (80.0, None), (None, True), (80.0, True), (None, False), (75.0, False))]
    assert [g is not None for g in got] == [False, False, False, False, True, True]
