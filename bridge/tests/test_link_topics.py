"""Horizontal History (link_topics.py): topics cut at pauses, titles from the owner's first words, bookmarks, routes."""

from __future__ import annotations

import asyncio
import json
from pathlib import Path

from test_link_chat import FakeInlet, at, http, serve, store_at

from eve_bridge import link_chat, link_topics
from eve_bridge.link_topics import Bookmarks, Titler, segment


def row(i: int, t: float, role: str = "owner", text: str = "hi") -> dict:
    return {"id": f"2026-10-13:{i}", "role": role, "text": text, "t": t}


def test_a_pause_longer_than_45_minutes_starts_a_new_topic_and_exactly_45_does_not() -> None:
    gap = link_topics.GAP_SECS
    rows = [row(0, 0, text="plan the trip"), row(1, 60, "eve", "Sure: where to?"),
            row(2, 60 + gap, text="Lisbon"),                       # exactly 45 min: same topic
            row(3, 60 + 2 * gap + 1, text="new thing")]            # longer: a new one
    a, b = segment(rows)
    assert (a["id"], a["last_id"], a["count"], a["title"]) == ("2026-10-13:0", "2026-10-13:2", 3, "plan the trip")
    for t in (a, b):
        t.pop("important"), t.pop("_lines")
    assert a["preview"] == "Sure: where to?" and (b["id"], b["count"]) == ("2026-10-13:3", 1)


def test_eves_own_lines_alone_are_not_a_topic() -> None:
    assert segment([row(0, 0, "eve", "Reminder: water the plants")]) == []


def test_a_hello_is_not_a_title_the_next_message_is() -> None:
    [t] = segment([row(0, 0, text="Hi eve!"), row(1, 5, "eve", "Hi!"), row(2, 30, text="what's on tomorrow?")])
    assert t["title"] == "what's on tomorrow?"
    [only] = segment([row(0, 0, text="hey"), row(1, 5, "eve", "Hey! What's up?")])
    assert only["title"] == "hey" and only["important"] is False             # nothing else said


def test_friendly_small_talk_is_small_talk() -> None:
    for hello in ("It's going amazing, how are you?", "What's good, friend?", "Hey friend.", "How are you bro?", "Are you there friend?", "How are u", "hi eve!", "what's up",
                  "how's it going", "I'm good", "thank you buddy"):
        [t] = segment([row(0, 0, text=hello), row(1, 5, "eve", "Hi!"), row(2, 9, text=hello),
                       row(3, 12, "eve", "Here.")])
        assert t["important"] is False, hello
    [t] = segment([row(0, 0, text="how are you at fixing bike chains when the link is bent?")])
    assert t["important"] is True                                               # long enough to be a question
    [t] = segment([row(0, 0, text="hey how do I fix the bike chain")])
    assert t["important"] is True                                               # a hello that asks something


def test_a_title_says_something() -> None:
    [t] = segment([row(0, 0, text="Is gofrai."), row(1, 5, "eve", "Sorry?"), row(2, 9, text="Is the garage open today")])
    assert t["title"] == "Is the garage open today"


def test_only_topics_with_substance_are_important() -> None:
    small = segment([row(0, 0, text="hi"), row(1, 5, "eve", "Hi!"), row(2, 9, text="thanks")])
    assert [t["important"] for t in small] == [False]                          # small talk, even three messages
    short = segment([row(0, 0, text="call mom")])
    assert [t["important"] for t in short] == [False]                          # two words, nothing back
    said = segment([row(0, 0, text="find me a quiet cafe near the office")])
    assert [t["important"] for t in said] == [True]                            # six words or more
    talk = segment([row(0, 0, text="call mom"), row(1, 5, "eve", "Calling."), row(2, 9, text="later"),
                    row(3, 12, "eve", "OK.")])
    assert [t["important"] for t in talk] == [True]                            # it went back and forth


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
    store.append("telegram", "t-1", "owner", "say", "plan the Lisbon trip for early November", now=at(day, "09:00"))
    store.append("telegram", "t-1", "eve", "say", "Three days works well.", now=at(day, "09:01"))
    store.append("telegram", "t-1", "owner", "say", "hey", now=at(day, "12:00"))         # small talk: no card
    store.append("telegram", "t-1", "eve", "say", "Hey!", now=at(day, "12:00"))
    store.append("telegram", "t-1", "owner", "say", "what's on my calendar for tomorrow?", now=at(day, "14:00"))
    store.append("telegram", "t-1", "eve", "say", "Two meetings.", now=at(day, "14:00"))

    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store), bookmarks=Bookmarks(tmp_path / "bm.json"),
                                          titler=Titler(tmp_path / "titles.json", environ={"EVE_TOPIC_TITLES": "0"}))
        status, _, body = await http(port, "GET", link_chat.TOPICS_PATH, token=token)
        view = json.loads(body)
        assert status == 200 and [t["title"] for t in view["topics"]] == ["what's on my calendar for tomorrow?",
                                                                      "plan the Lisbon trip for early November"]
        first = view["topics"][1]
        assert set(first) == {"id", "first_id", "last_id", "started_at", "ended_at", "count", "title", "preview",
                              "bookmarked"} and first["bookmarked"] is False and view["bookmarked"] == []

        status, _, body = await http(port, "POST", link_chat.TOPICS_BOOKMARK_PATH, {"id": first["id"], "on": True},
                                     token=token)
        assert status == 200 and json.loads(body) == {"ok": True, "bookmarked": True}
        view = json.loads((await http(port, "GET", link_chat.TOPICS_PATH, token=token))[2])
        assert [t["title"] for t in view["bookmarked"]] == ["plan the Lisbon trip for early November"]
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


def test_model_titles_are_cached_by_count_and_small_talk_hides_a_card(tmp_path: Path) -> None:
    asked: list[str] = []

    def ask(system: str, text: str) -> dict:
        asked.append(text)
        if "Lisbon" in text:
            return {"title": "Lisbon trip in November.", "small_talk": False}
        return {"title": "Checking in", "small_talk": True}

    titler = Titler(tmp_path / "chat" / "titles.json", environ={}, ask=ask)
    topics = segment([row(0, 0, text="can you plan a three day Lisbon trip"), row(1, 5, "eve", "Sure."),
                      row(2, 5000, text="are you around this evening or busy with things"), row(3, 5005, "eve", "Here.")])
    assert titler.title_pass(topics) == 2 and len(asked) == 2
    assert "Person: can you plan a three day Lisbon trip" in asked[0] or "Person: can you plan" in asked[1]
    assert (titler.path.stat().st_mode & 0o777) == 0o600
    titler.apply(topics)
    by_id = {t["id"]: t for t in topics}
    assert by_id["2026-10-13:0"]["title"] == "Lisbon trip in November" and by_id["2026-10-13:0"]["important"] is True
    assert by_id["2026-10-13:2"]["important"] is False                          # the model called it small talk
    assert titler.title_pass(topics) == 0 and len(asked) == 2                   # cached: no second call
    grown = segment([row(0, 0, text="can you plan a three day Lisbon trip"), row(1, 5, "eve", "Sure."),
                     row(9, 60, text="add Porto for one night")])
    assert titler.title_pass(grown) == 1 and len(asked) == 3                    # it grew: titled again


def test_a_failing_model_keeps_the_first_message_title(tmp_path: Path) -> None:
    def ask(system: str, text: str) -> dict:
        raise TimeoutError

    titler = Titler(tmp_path / "titles.json", environ={}, ask=ask)
    topics = segment([row(0, 0, text="can you plan a three day Lisbon trip")])
    assert titler.title_pass(topics) == 0 and not titler.path.exists()
    titler.apply(topics)
    assert topics[0]["title"] == "can you plan a three day Lisbon trip"


def test_titles_off_never_ask(tmp_path: Path) -> None:
    titler = Titler(tmp_path / "titles.json", environ={"EVE_TOPIC_TITLES": "0"}, ask=lambda s, t: {"title": "x"})
    assert titler.kick(segment([row(0, 0, text="can you plan a three day Lisbon trip")])) is False


def test_a_list_titles_in_the_background_without_racing_the_reply(tmp_path: Path) -> None:
    store = store_at(tmp_path)
    store.append("telegram", "t-1", "owner", "say", "plan the Lisbon trip for early November", now=at("2026-10-13", "09:00"))
    titler = Titler(tmp_path / "titles.json", environ={}, ask=lambda s, t: {"title": "Lisbon trip", "small_talk": False})
    first = link_topics.view(store, Bookmarks(tmp_path / "bm.json"), titler=titler)
    assert "_lines" not in first["topics"][0] and titler.thread is not None
    titler.thread.join(5)
    assert link_topics.view(store, Bookmarks(tmp_path / "bm.json"), titler=titler)["topics"][0]["title"] == "Lisbon trip"


# ---- infinite scroll: ?before=<message id> ------------------------------------------------------------------------

def test_older_pages_reach_the_first_day_in_order(tmp_path: Path) -> None:
    from eve_bridge.link_chat import HISTORY_LIMIT, read_before, read_messages

    store = store_at(tmp_path)
    for d in range(1, 6):                                       # five days, 60 lines each: 300 lines
        for k in range(60):
            store.append("telegram", "t-1", "owner" if k % 2 == 0 else "eve", "say", f"day {d} line {k}",
                         now=at(f"2026-10-0{d}", f"{10 + k // 60}:{k % 60:02d}"))
    first, cursor = read_messages(store)
    assert len(first) == HISTORY_LIMIT and first[-1]["text"] == "day 5 line 59"          # beyond the last 3 days rule
    assert cursor.startswith("2026-10-05:")
    seen = list(first)
    before, more = first[0]["id"], True
    while more:
        page, more = read_before(store, before)
        assert page and page[-1]["t"] <= seen[0]["t"]
        seen = page + seen
        before = page[0]["id"]
    assert len(seen) == 300 and seen[0]["text"] == "day 1 line 0"
    assert [r["id"] for r in seen] == sorted({r["id"] for r in seen}, key=lambda i: (i.split(":")[0], int(i.split(":")[1])))


def test_the_older_route_answers_a_page_and_refuses_a_bad_id(tmp_path: Path) -> None:
    store = store_at(tmp_path)
    for k in range(5):
        store.append("telegram", "t-1", "owner", "say", f"line {k}", now=at("2026-10-13", f"09:0{k}"))

    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store))
        status, _, body = await http(port, "GET", link_chat.MESSAGES_PATH + "?before=2026-10-13:3", token=token)
        got = json.loads(body)
        assert status == 200 and [m["text"] for m in got["messages"]] == ["line 0", "line 1", "line 2"]
        assert got["more"] is False and got["cursor"] == ""
        for bad in ("x", "2026-10-13:-1", "../a"):
            assert (await http(port, "GET", link_chat.MESSAGES_PATH + f"?before={bad}", token=token))[0] == 400
        await server.close()

    asyncio.run(go())
