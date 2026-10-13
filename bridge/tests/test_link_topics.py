"""Horizontal History (link_topics.py): topics cut at pauses, titles from the owner's first words, bookmarks, routes."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path

from test_link_chat import FakeInlet, at, http, serve, store_at

from eve_bridge import link_chat, link_topics
from eve_bridge.link_topics import Bookmarks, segment


def row(i: int, t: float, role: str = "owner", text: str = "hi") -> dict:
    return {"id": f"2026-10-13:{i}", "role": role, "text": text, "t": t}


def test_a_pause_longer_than_45_minutes_starts_a_new_topic_and_exactly_45_does_not() -> None:
    gap = link_topics.GAP_SECS
    rows = [row(0, 0, text="plan the trip"), row(1, 60, "eve", "Sure: where to?"),
            row(2, 60 + gap, text="Lisbon"),                       # exactly 45 min: same topic
            row(3, 60 + 2 * gap + 1, text="new thing")]            # longer: a new one
    a, b = segment(rows)
    assert (a["id"], a["last_id"], a["count"], a["title"]) == ("2026-10-13:0", "2026-10-13:2", 3, "plan the trip")
    assert a["preview"] == "Sure: where to?" and (b["id"], b["count"]) == ("2026-10-13:3", 1)


def test_eves_own_lines_alone_are_not_a_topic() -> None:
    assert segment([row(0, 0, "eve", "Reminder: water the plants")]) == []


def test_titles_are_plain_and_clipped_at_a_word() -> None:
    long = "**Can you** help me `figure out` " + "the best way to organise everything " * 4
    [t] = segment([row(0, 0, text=long)])
    assert t["title"].startswith("Can you help me figure out the best") and t["title"].endswith("…")
    assert len(t["title"]) <= link_topics.TITLE_CHARS and "*" not in t["title"] and "`" not in t["title"]


def test_bookmarks_are_private_survive_and_unset(tmp_path: Path) -> None:
    b = Bookmarks(tmp_path / "chat" / "bookmarks.json")
    b.set("2026-10-13:0", True, now=10)
    b.set("2026-10-13:0", True, now=99)                                      # saving again keeps the first time
    assert b.load() == {"2026-10-13:0": 10.0}
    assert (b.path.stat().st_mode & 0o777) == 0o600
    b.set("2026-10-13:0", False)
    assert b.load() == {}
    b.path.write_text("not json")
    assert b.load() == {}                                                    # a damaged file is no bookmarks


def test_routes_list_save_and_refuse(tmp_path: Path) -> None:
    store = store_at(tmp_path)
    day = "2026-10-13"
    store.append("telegram", "t-1", "owner", "say", "plan the Lisbon trip", now=at(day, "09:00"))
    store.append("telegram", "t-1", "eve", "say", "Three days works well.", now=at(day, "09:01"))
    store.append("telegram", "t-1", "owner", "say", "what's on tomorrow?", now=at(day, "14:00"))
    store.append("telegram", "t-1", "eve", "say", "Two meetings.", now=at(day, "14:00"))

    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store), bookmarks=Bookmarks(tmp_path / "bm.json"))
        status, _, body = await http(port, "GET", link_chat.TOPICS_PATH, token=token)
        view = json.loads(body)
        assert status == 200 and [t["title"] for t in view["topics"]] == ["what's on tomorrow?", "plan the Lisbon trip"]
        first = view["topics"][1]
        assert set(first) == {"id", "first_id", "last_id", "started_at", "ended_at", "count", "title", "preview",
                              "bookmarked"} and first["bookmarked"] is False and view["bookmarked"] == []

        status, _, body = await http(port, "POST", link_chat.TOPICS_BOOKMARK_PATH, {"id": first["id"], "on": True},
                                     token=token)
        assert status == 200 and json.loads(body) == {"ok": True, "bookmarked": True}
        view = json.loads((await http(port, "GET", link_chat.TOPICS_PATH, token=token))[2])
        assert [t["title"] for t in view["bookmarked"]] == ["plan the Lisbon trip"]
        assert view["topics"][1]["bookmarked"] is True

        assert (await http(port, "GET", link_chat.TOPICS_PATH + "?limit=1", token=token))[0] == 200
        assert len(json.loads((await http(port, "GET", link_chat.TOPICS_PATH + "?limit=1", token=token))[2])["topics"]) == 1
        assert (await http(port, "GET", link_chat.TOPICS_PATH + "?limit=x", token=token))[0] == 400
        for bad in ({}, {"id": "../etc", "on": True}, {"id": first["id"], "on": "yes"}):
            assert (await http(port, "POST", link_chat.TOPICS_BOOKMARK_PATH, bad, token=token))[0] == 400, bad
        assert (await http(port, "POST", link_chat.TOPICS_BOOKMARK_PATH, {"id": "2026-01-01:0", "on": True},
                           token=token))[0] == 404
        assert (await http(port, "GET", link_chat.TOPICS_PATH))[0] == 401                       # no token
        assert (await http(port, "GET", link_chat.TOPICS_PATH, token=token,
                           origin="https://example.com"))[0] == 403               # a browser
        assert (await http(port, "POST", link_chat.TOPICS_PATH, {}, token=token))[0] == 405
        await server.close()

    asyncio.run(go())
