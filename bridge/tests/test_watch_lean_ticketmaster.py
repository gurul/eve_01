"""Replay tests for the Ticketmaster watch, from the Lean model verification/Eve/WatchTicketmaster.lean and a
review of pick_attraction / TRIBUTE. Each test fails on the code as it was when the model was written, for exactly
the bug it names. No network: every request goes to a fake."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import Any

import pytest
from test_watch import Clock, make_watcher, tm_event

from eve_bridge import watch
from eve_bridge.watch import FetchError


def tm_add(w: watch.Watcher, target: str, label: str) -> dict[str, Any]:
    return asyncio.run(w.add({"kind": "ticketmaster", "target": target, "label": label, "condition": "available",
                              "value": None, "text": None, "city": None, "field": None, "every_minutes": None}))


# ---- Lean: first_read_after_failed_add (WatchTicketmaster.current_violates) --------------------------------

def test_a_sale_open_at_the_first_background_check_is_texted(tmp_path: Path,
                                                               monkeypatch: pytest.MonkeyPatch) -> None:
    """The owner adds a watch while Ticketmaster's API is down, so the reply says "the first check failed" and
    shows no reading. A presale then opens. The first check that succeeds finds tickets on sale: the owner has
    never been told, so that check must text them. The current evaluate treats every first reading as already
    shown (it only arms), so the owner hears nothing about this sale at all."""
    monkeypatch.setenv("TICKETMASTER_API_KEY", "tm-test")
    now = 1_800_000_000
    clock = Clock(now)
    sent: list[str] = []
    state = {"down": True}

    async def notify(text: str) -> None:
        sent.append(text)

    attractions = {"_embedded": {"attractions": [{"id": "K8vZ", "name": "Big Band",
                                                  "upcomingEvents": {"_total": 1}}]}}

    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        if state["down"]:
            raise FetchError("the site answered HTTP 503", 503)
        if "/attractions.json" in url:
            return 200, json.dumps(attractions)
        # a presale open since a minute ago; the public sale a day away
        event = tm_event("offsale", public=(clock.t + 86400, None), presale=(clock.t - 60, clock.t + 3600))
        return 200, json.dumps({"_embedded": {"events": [event]}})

    w = make_watcher(tmp_path / "w.json", fetch=fetch, clock=clock, notify=notify, ticketmaster=True)
    out = tm_add(w, "Big Band", "Big Band")
    assert out["ok"] and out["now"].startswith("the first check failed")
    item = w.watches[0]
    assert item.checks == 0
    state["down"] = False
    w.limiter.clear(watch.TM_HOST)
    clock.t = item.next_at + 1
    asyncio.run(w.tick())
    assert item.checks == 1 and item.last_available is True
    assert len(sent) == 1 and sent[0].startswith("Big Band is available now.")


def test_a_reading_shown_in_the_add_reply_still_never_texts(tmp_path: Path,
                                                            monkeypatch: pytest.MonkeyPatch) -> None:
    """The other half of the same rule, kept by the fix: when the add reply showed the sale open, the owner
    is not texted about that same open stretch."""
    monkeypatch.setenv("TICKETMASTER_API_KEY", "tm-test")
    now = 1_800_000_000
    clock = Clock(now)
    sent: list[str] = []

    async def notify(text: str) -> None:
        sent.append(text)

    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        if "/attractions.json" in url:
            return 200, json.dumps({"_embedded": {"attractions": [{"id": "K8vZ", "name": "Big Band"}]}})
        return 200, json.dumps({"_embedded": {"events": [tm_event("onsale")]}})

    w = make_watcher(tmp_path / "w.json", fetch=fetch, clock=clock, notify=notify, ticketmaster=True)
    out = tm_add(w, "Big Band", "Big Band")
    assert out["now"] == "available" and "already" in out
    for _ in range(3):
        clock.t = w.watches[0].next_at + 1
        asyncio.run(w.tick())
    assert sent == []


# ---- review: TRIBUTE false negatives -----------------------------------------------------------------------

def test_an_exact_name_with_a_tribute_word_is_the_act_itself() -> None:
    """The owner names "The Jimi Hendrix Experience" exactly. That attraction is dropped as a tribute act only
    because its name contains "Experience", so the watch never finds the band. A tribute word the owner typed
    is part of the name they mean; a tribute act they did not name is still skipped."""
    payload = {"_embedded": {"attractions": [
        {"id": "K1", "name": "The Jimi Hendrix Experience", "upcomingEvents": {"_total": 3}},
        {"id": "T1", "name": "Experience Unlimited", "upcomingEvents": {"_total": 9}}]}}
    assert watch.pick_attraction("The Jimi Hendrix Experience", payload) == "K1"
    assert watch.pick_attraction("Experience Unlimited", payload) == "T1"
    # still skipped: the name adds a tribute word the owner's words lack
    tribute = {"_embedded": {"attractions": [
        {"id": "T2", "name": "Jimi Hendrix Experience Tribute", "aliases": ["the jimi hendrix experience"]}]}}
    assert watch.pick_attraction("The Jimi Hendrix Experience", tribute) == ""
    big = {"_embedded": {"attractions": [{"id": "T3", "name": "Big Band Tribute", "aliases": ["big band"]}]}}
    assert watch.pick_attraction("big band", big) == ""


def test_the_keyword_search_keeps_games(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    """With no exact attraction, the keyword search drops every event whose name matches TRIBUTE. A game on
    Ticketmaster is named "Golden State Warriors vs. Los Angeles Lakers", so every game is dropped and the watch
    says nothing is listed while the game is on sale."""
    monkeypatch.setenv("TICKETMASTER_API_KEY", "tm-test")
    now = 1_800_000_000
    game = tm_event("onsale")
    game["name"] = "Golden State Warriors vs. Los Angeles Lakers"
    game["classifications"] = [{"segment": {"name": "Sports"}}]
    show = tm_event("onsale")
    show["name"] = "Beatles vs Stones - A Musical Showdown"
    show["classifications"] = [{"segment": {"name": "Music"}}]

    def fetch(url: str, **kw: Any) -> tuple[int, str]:
        if "/attractions.json" in url:
            return 200, json.dumps({"_embedded": {"attractions": []}})
        return 200, json.dumps({"_embedded": {"events": [game]}})

    w = make_watcher(tmp_path / "w.json", fetch=fetch, clock=Clock(now), ticketmaster=True)
    for words in ("Warriors vs Lakers", "LA Lakers"):
        item = watch.Watch(id="w1", kind="ticketmaster", condition="available", label=words, target=words)
        r = asyncio.run(w.read(item))
        assert item.ref == "-" and r.available is True, words

    def fetch_show(url: str, **kw: Any) -> tuple[int, str]:
        if "/attractions.json" in url:
            return 200, json.dumps({"_embedded": {"attractions": []}})
        return 200, json.dumps({"_embedded": {"events": [show]}})

    # a music tribute billed "X vs Y" is still dropped when the owner asked for the band
    w2 = make_watcher(tmp_path / "w2.json", fetch=fetch_show, clock=Clock(now), ticketmaster=True)
    item = watch.Watch(id="w2", kind="ticketmaster", condition="available", label="Beatles", target="Beatles")
    assert asyncio.run(w2.read(item)).available is False
