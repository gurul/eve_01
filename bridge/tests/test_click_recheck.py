"""A click is refused when the control under its id is no longer the one the decision was made on.

The snapshot records each control's role, name and href, and the document's URL and performance.timeOrigin.
At the click the page is read again; any difference refuses with "refused: stale:" and nothing is clicked, so
the loop looks again instead of pressing whatever now answers to that id. (After LayaBrowse's
decision_still_valid, MIT.)
"""

from __future__ import annotations

import asyncio
from typing import Any, Callable

import pytest

from browser_fakes import FakePage, button, lane_on, link
from eve_bridge import browser_lane as bl


def news_page() -> FakePage:
    return FakePage("https://news.example/", "News", "Top stories today",
                    [link("0", "Top stories", "https://news.example/top"), button("1", "Subscribe"),
                     link("2", "World", "https://news.example/world")])


def set_field(eid: str, key: str, value: Any) -> Callable[[FakePage], None]:
    def go(page: FakePage) -> None:
        page.element(eid)[key] = value
    return go


def navigated(page: FakePage) -> None:
    page.url = "https://news.example/other"


def reloaded(page: FakePage) -> None:
    page.doc += 7.0                                    # same URL, a new document (a reload, a client re-render)


def renumbered(page: FakePage) -> None:
    # A re-render put a different control under id 0: same URL, same document, a different element.
    page.elements[0] = button("0", "Unsubscribe")


def gone(page: FakePage) -> None:
    page.elements = [e for e in page.elements if e["id"] != "0"]


@pytest.mark.parametrize(("mutate", "what"), [
    (set_field("0", "role", "button"), "role"),
    (set_field("0", "name", "Sponsored"), "name"),
    (set_field("0", "href", "https://ads.example/x"), "href"),
    (navigated, "page address"),
    (reloaded, "document"),
    (renumbered, "role"),
])
def test_a_changed_control_is_refused_and_not_clicked(mutate: Callable[[FakePage], None], what: str) -> None:
    page = news_page()
    p = bl._Page(page)
    snap = p.snapshot()
    target = next(c for c in snap.elements if c.label == "Top stories")
    page.mutate = mutate
    said = p.click_candidate(target)
    assert said.startswith(bl.STALE_PREFIX), said
    assert what in said
    assert page.clicks == []
    assert page.rechecks == 1


def test_a_control_that_vanished_is_refused() -> None:
    page = news_page()
    p = bl._Page(page)
    target = next(c for c in p.snapshot().elements if c.label == "Top stories")
    page.mutate = gone
    said = p.click_candidate(target)
    assert said.startswith("refused:") and page.clicks == []


def test_an_unchanged_control_is_clicked_even_when_other_text_moved() -> None:
    page = news_page()
    p = bl._Page(page)
    target = next(c for c in p.snapshot().elements if c.label == "Top stories")

    def churn(pg: FakePage) -> None:                   # a live page: new text, another control renamed
        pg.body += " Breaking: more news"
        pg.element("2")["name"] = "World (3 new)"

    page.mutate = churn
    said = p.click_candidate(target)
    assert said.startswith("clicked link 'Top stories'"), said
    assert page.clicks == ["0"] and page.rechecks == 1


def test_an_unreadable_recheck_refuses() -> None:
    page = news_page()
    p = bl._Page(page)
    target = next(c for c in p.snapshot().elements if c.label == "World")
    original = page.evaluate

    def broken(script: str, arg: Any = None) -> Any:
        if script == bl._RECHECK_JS:
            raise RuntimeError("Execution context was destroyed, most likely because of a navigation")
        return original(script, arg)

    page.evaluate = broken
    said = p.click_candidate(target)
    assert said.startswith(bl.STALE_PREFIX) and page.clicks == []


@pytest.mark.parametrize(("recorded", "now", "changed"), [
    ({"url": "u", "doc": "1", "role": "link", "name": "A", "href": "h"},
     {"url": "u", "doc": "1", "found": True, "role": "link", "name": "A", "href": "h"}, []),
    ({"url": "u", "doc": "1", "role": "link", "name": "A", "href": "h"},
     {"url": "u2", "doc": "2", "found": True, "role": "button", "name": "B", "href": ""},
     ["page address", "document", "role", "name", "href"]),
    ({"url": "u", "doc": "1", "role": "link", "name": "A", "href": "h"}, {"url": "u", "doc": "1", "found": False},
     ["control"]),
    ({"url": "u", "doc": "1", "role": "link", "name": "A", "href": "h"}, "not a dict", ["control"]),
])
def test_click_recheck_is_a_pure_comparison(recorded: dict[str, Any], now: Any, changed: list[str]) -> None:
    assert bl.click_recheck(recorded, now) == changed


def test_the_executor_stops_on_a_stale_click_and_presses_nothing(tmp_path: Any) -> None:
    page = FakePage("https://news.example/", "News", "Top stories today",
                    [button("0", "Show more"), button("1", "Print")])
    lane = lane_on(page, tmp_path)
    page.mutate = set_field("0", "name", "Show less")          # between the decision and the click
    plan = {"steps": [{"kind": "click", "target": "the Show more button", "label_hint": "Show more"}],
            "final_say": "Shown.", "success": {"kind": "text_visible", "value": "Top stories"}}

    async def go() -> dict[str, Any]:
        try:
            return await lane.run_plan(plan, "show more stories on news.example")
        finally:
            await lane.close()

    result = asyncio.run(go())
    assert result["status"] in ("partial", "none"), result
    assert page.clicks == [] and page.rechecks == 1
    assert result["ledger"][0]["effect"] == "refused", result
