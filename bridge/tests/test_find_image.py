"""find_image.py: a real, photo-sized picture from Wikipedia for a card, or a plain no; events carry Ticketmaster's."""

from __future__ import annotations

import asyncio
import os

import pytest

from eve_bridge import events, find_image


def page(src: str, w: int, h: int, title: str = "Climate Pledge Arena", index: int = 1) -> dict:
    return {"title": title, "index": index, "original": {"source": src, "width": w, "height": h}}


def test_a_photo_sized_picture_is_kept_with_its_page() -> None:
    got = find_image.pick({"query": {"pages": [page("https://upload.wikimedia.org/a/Arena.jpg", 2400, 1600)]}})
    assert got == {"ok": True, "image": "https://upload.wikimedia.org/a/Arena.jpg", "width": 2400, "height": 1600,
                   "page": "Climate Pledge Arena", "page_url": "https://en.wikipedia.org/wiki/Climate_Pledge_Arena"}


@pytest.mark.parametrize("src,w,h", [("https://upload.wikimedia.org/a/Logo.svg", 3000, 3000),     # drawn
                                     ("https://upload.wikimedia.org/a/tiny.jpg", 320, 240),        # small
                                     ("http://upload.wikimedia.org/a/Arena.jpg", 2400, 1600)])     # not https
def test_a_drawn_small_or_plain_http_picture_is_refused(src: str, w: int, h: int) -> None:
    assert find_image.pick({"query": {"pages": [page(src, w, h)]}}) is None


def test_the_best_ranked_good_page_wins() -> None:
    pages = [page("https://u.org/b.jpg", 1600, 900, "Second", 2), page("https://u.org/a.svg", 900, 900, "First", 1)]
    assert find_image.pick({"query": {"pages": pages}})["page"] == "Second"      # the first has only a drawing


def test_handle_says_no_plainly_and_never_raises() -> None:
    async def down(url: str, params: dict) -> dict:
        raise TimeoutError

    async def empty(url: str, params: dict) -> dict:
        return {}

    got = asyncio.run(find_image.handle("find_image", {"subject": "Pike Place Market"}, environ={}, http=down))
    assert got["ok"] is False and "leave the picture out" in got["reason"]
    assert asyncio.run(find_image.handle("find_image", {"subject": "x"}, environ={}, http=empty))["ok"] is False
    off = asyncio.run(find_image.handle("find_image", {"subject": "Pike Place"}, environ={"EVE_FIND_IMAGE": "0"}))
    assert off == {"ok": False, "reason": find_image.OFF_REASON} and find_image.tools({"EVE_FIND_IMAGE": "0"}) == []


def test_an_event_keeps_ticketmasters_widest_16_9_picture() -> None:
    imgs = [{"url": "https://s1.ticketm.net/a_3_2.jpg", "ratio": "3_2", "width": 3000},
            {"url": "https://s1.ticketm.net/a_16_9_S.jpg", "ratio": "16_9", "width": 640},
            {"url": "https://s1.ticketm.net/a_16_9_L.jpg", "ratio": "16_9", "width": 2048},
            {"url": "https://s1.ticketm.net/fallback.jpg", "ratio": "16_9", "width": 4000, "fallback": True}]
    assert events.trim({"name": "Show", "images": imgs})["image"] == "https://s1.ticketm.net/a_16_9_L.jpg"
    assert "image" not in events.trim({"name": "Show", "images": [imgs[0]]})


@pytest.mark.live
@pytest.mark.skipif(os.environ.get("EVE_LIVE") != "1", reason="needs EVE_LIVE=1 and the network")
def test_live_wikipedia_gives_a_landmark_its_picture() -> None:
    got = asyncio.run(find_image.handle("find_image", {"subject": "Space Needle"}, environ={}))
    assert got["ok"] is True and got["image"].startswith("https://upload.wikimedia.org/") and got["width"] >= 800


def test_the_brain_is_offered_the_tool_when_on() -> None:
    assert [t["name"] for t in find_image.tools({})] == ["find_image"]


def test_only_an_app_turn_is_offered_the_picture_tool(monkeypatch: pytest.MonkeyPatch) -> None:
    from test_telegram import FakeApi, FakeCreate, Rig

    from eve_bridge import telegram

    monkeypatch.setenv("EVE_FIND_IMAGE", "1")
    rig = Rig(FakeApi(), FakeCreate())
    spoken = rig.inlet._tool_lists(set(), set(), False)["allowed"]
    mark = telegram._FROM_APP.set("t-1")
    try:
        typed = rig.inlet._tool_lists(set(), set(), False)["allowed"]
    finally:
        telegram._FROM_APP.reset(mark)
    assert "find_image" in typed and "find_image" not in spoken           # a call is spoken: no picture tool
