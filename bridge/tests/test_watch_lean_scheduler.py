"""Replays of the scheduler traces that verification/Eve/WatchScheduler.lean found (watch.py's Watcher).

Each test drives one interleaving the Lean model names, with the real Watcher and fakes lent for the
network, so no test touches a service. The fetch runs off the loop (offload=True), exactly as in the
daemon, and a gate holds it mid-flight while the owner's tool call runs on the loop.
"""

from __future__ import annotations

import asyncio
import json
import random
import threading
from pathlib import Path
from typing import Any, Optional

from eve_bridge.watch import FetchError, RateLimiter, Watch, WatchConfig, Watcher


class Clock:
    def __init__(self, t: float = 1_800_000_000.0) -> None:
        self.t = t

    def __call__(self) -> float:
        return self.t


def quote_body(price: float) -> str:
    return json.dumps({"chart": {"result": [{"meta": {"symbol": "AAPL", "currency": "USD",
                                                       "regularMarketPrice": price, "shortName": "Apple Inc."}}]}})


class GatedFetch:
    """A fetch that answers from ``script`` (a price, or a FetchError to raise). When ``hold`` is set, the next
    call signals ``entered`` and blocks until ``release``: the check is then mid-flight."""

    def __init__(self, script: list[Any]) -> None:
        self.script = list(script)
        self.calls = 0
        self.hold = False
        self.entered = threading.Event()
        self.release = threading.Event()

    def __call__(self, url: str, **kw: Any) -> tuple[int, str]:
        self.calls += 1
        if self.hold:
            self.hold = False
            self.entered.set()
            assert self.release.wait(5), "the test never released the fetch"
        item = self.script.pop(0)
        if isinstance(item, FetchError):
            raise item
        return 200, quote_body(item)


def make(path: Path, fetch: Any, sent: list[str], clock: Optional[Clock] = None,
         ask: Any = None) -> Watcher:
    clock = clock or Clock()

    async def notify(text: str) -> None:
        sent.append(text)

    async def no_sleep(_s: float) -> None:
        return None

    lim = RateLimiter(rate_per_min=600, burst=50, host_gap_secs=0, backoff_base=60, backoff_max=600,
                      model_calls_per_day=50, clock=clock, day=lambda t: str(int(t // 86400)))
    return Watcher(WatchConfig(enabled=True, path=path), notify=notify, fetch=fetch,
                   ask=ask or (lambda body: {}), clock=clock, sleep=no_sleep, rng=random.Random(0),
                   limiter=lim, offload=True, resolve=lambda host: host.endswith(".example"))


def add_apple(w: Watcher, condition: str = "below") -> dict[str, Any]:
    return asyncio.run(w.add({"kind": "quote", "target": "AAPL", "label": "Apple", "condition": condition,
                              "value": 300 if condition == "below" else None, "text": None, "every_minutes": 5}))


async def remove_mid_check(w: Watcher, fetch: GatedFetch, wid: str) -> dict[str, Any]:
    """tick picks the watch and its fetch starts; the owner's watch_remove runs; then the fetch answers."""
    fetch.hold = True
    tick = asyncio.create_task(w.tick())
    while not fetch.entered.is_set():
        await asyncio.sleep(0.005)
    out = await w.handle("watch_remove", {"id": wid})     # the Telegram brain's tool call, on the loop
    fetch.release.set()
    await tick
    return out


# ---- Lean: current_violates (a removed watch's alert is texted) -------------------------------

def test_an_alert_in_flight_is_not_texted_after_the_watch_is_removed(tmp_path: Path) -> None:
    """Lean trace [validate, queue, pick 0, enter, remove 1, ret alert, send]: the watch is removed while its
    check is reading; the reading crosses the mark; the alert must not reach the owner, who just stopped it."""
    sent: list[str] = []
    clock = Clock()
    fetch = GatedFetch([310, 299])
    w = make(tmp_path / "w.json", fetch, sent, clock)
    assert add_apple(w)["id"] == "w1"
    clock.t += 400                                         # past the next check (5 min ± 10%)
    out = asyncio.run(remove_mid_check(w, fetch, "w1"))
    assert out == {"ok": True, "removed": "Apple"}
    assert w.watches == [] and fetch.calls == 2            # the check was already reading when it was removed
    assert sent == [], f"a removed watch texted the owner: {sent}"


def test_an_error_notice_in_flight_is_not_texted_after_the_watch_is_removed(tmp_path: Path) -> None:
    """The same trace with the fourth failure in a row: "I can't read Apple" is not sent for a removed watch."""
    sent: list[str] = []
    clock = Clock()
    fetch = GatedFetch([310, FetchError("the site answered HTTP 500", 500)])
    w = make(tmp_path / "w.json", fetch, sent, clock)
    add_apple(w, "change")
    w.watches[0].errors = 3                                # three failures already in this streak
    clock.t += 400
    asyncio.run(remove_mid_check(w, fetch, "w1"))
    assert w.watches == []
    assert sent == [], f"a removed watch texted the owner: {sent}"


def test_a_recovery_notice_in_flight_is_not_texted_after_the_watch_is_removed(tmp_path: Path) -> None:
    """A told streak ends while the watch is being removed: "I can read Apple again" is not sent either."""
    sent: list[str] = []
    clock = Clock()
    fetch = GatedFetch([310, 305])
    w = make(tmp_path / "w.json", fetch, sent, clock)
    add_apple(w, "change")
    w.watches[0].errors, w.watches[0].told_error = 5, True   # the owner was told it cannot be read
    w.watches[0].next_at = clock.t
    asyncio.run(remove_mid_check(w, fetch, "w1"))
    assert w.watches == []
    assert sent == [], f"a removed watch texted the owner: {sent}"


# ---- Lean: current_reuses_id (an id the owner was given names a second watch) -----------------

def test_a_removed_watch_id_is_never_handed_out_again(tmp_path: Path) -> None:
    """Lean trace [validate, queue, remove 1, validate, queue]: the second watch gets the removed watch's id, so
    "stop watching w1" (from the owner's earlier /watches listing, or the transcript) would now stop the new one."""
    sent: list[str] = []
    path = tmp_path / "w.json"
    w = make(path, GatedFetch([310, 310, 310]), sent)
    first = add_apple(w)["id"]
    assert w.remove(first)["ok"]
    second = add_apple(w)["id"]
    assert second != first, f"the removed watch's id {first} was handed out again"
    # and not after a restart either: the store remembers the ids it has handed out
    assert w.remove(second)["ok"]
    again = make(path, GatedFetch([310]), sent)
    third = add_apple(again)["id"]
    assert third not in (first, second), f"a restart handed out {third} again"


# ---- Lean: starve_current_violates (one watch whose check raises stops every watch after it) --

async def ticks_like_the_loop(w: Watcher, clock: Clock, n: int) -> None:
    for _ in range(n):                                     # run(): a tick that raises is logged, then retried
        try:
            await w.tick()
        except Exception:  # noqa: BLE001
            pass
        clock.t += 60


def test_a_rule_that_raises_does_not_starve_the_watches_after_it(tmp_path: Path) -> None:
    """Lean trace starve_current_violates, with a real page: it states no price in its markup, and the page-text
    model reads it as 0 (model_reading keeps any price >= 0), so a drop_pct watch keeps 0 as its baseline and
    evaluate() divides by it on the next check (ZeroDivisionError, not FetchError). That raise leaves tick with
    next_at unmoved, so the page is first in line on every tick, fetched once a minute, and the quote watch due
    after it is never checked."""
    sent: list[str] = []
    clock = Clock()
    fetched: list[str] = []

    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        fetched.append(url)
        return 200, (quote_body(305) if "yahoo" in url else "<html><body><h1>Lamp</h1><p>Free today</p></body></html>")

    def ask(body: dict[str, Any]) -> dict[str, Any]:
        return {"choices": [{"message": {"content": json.dumps({"price": 0, "currency": "USD", "available": True,
                                                                 "note": "Listed as free."})}}]}

    w = make(tmp_path / "w.json", fetch, sent, clock, ask=ask)
    out = asyncio.run(w.add({"kind": "page", "target": "https://shop.example/lamp", "label": "Lamp",
                             "condition": "drop_pct", "value": 10, "text": None, "every_minutes": None}))
    assert out["ok"] and w.watches[0].baseline == 0.0
    add_apple(w, "change")
    lamp, apple = w.watches
    lamp.next_at, apple.next_at = clock.t - 10, clock.t     # both due; the lamp first in next_at order
    checks = apple.checks
    asyncio.run(ticks_like_the_loop(w, clock, 3))
    assert apple.checks > checks, "the quote watch was never checked: the raising watch starved it"
    assert sum("shop.example" in u for u in fetched) == 2, f"the raising page was refetched every tick: {fetched}"


def test_a_reader_that_raises_does_not_starve_the_watches_after_it(tmp_path: Path) -> None:
    """The same trace from a reader: a search watch whose model reply has a string where the message object goes
    (choices[0].message), so _reply_text raises AttributeError, not FetchError."""
    sent: list[str] = []
    clock = Clock()
    fetch = GatedFetch([310, 305, 305])
    asks: list[dict[str, Any]] = []

    def ask(body: dict[str, Any]) -> dict[str, Any]:
        asks.append(body)
        return {"choices": [{"message": "upstream hiccup"}]}

    w = make(tmp_path / "w.json", fetch, sent, clock, ask=ask)
    add_apple(w, "change")
    apple = w.watches[0]
    broken = Watch(id="w2", kind="search", target="tour tickets", label="Tour", condition="available",
                   every_secs=3600, next_at=clock.t - 10)
    w.watches.append(broken)
    apple.next_at = clock.t                                # due, after the broken one in next_at order
    checks = apple.checks
    asyncio.run(ticks_like_the_loop(w, clock, 3))
    assert apple.checks > checks, "the quote watch was never checked: the raising watch starved it"
    assert len(asks) == 1, f"the raising watch was asked again on every tick ({len(asks)} model calls)"
