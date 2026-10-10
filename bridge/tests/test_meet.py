"""meet.py: links, the caption merger, one call's state machine, the calendar and the notes. No network, no
browser: the page is a script of states (FakePage), the clock is a counter, sleep advances it."""
from __future__ import annotations

import asyncio
import json
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import meet

# ---- G3: links -----------------------------------------------------------------------------------


@pytest.mark.parametrize("text,url", [
    ("https://meet.google.com/abc-defg-hij", "https://meet.google.com/abc-defg-hij?hl=en"),
    ("join meet.google.com/abc-defg-hij please", "https://meet.google.com/abc-defg-hij?hl=en"),
    ("<https://meet.google.com/ABC-DEFG-HIJ/>", "https://meet.google.com/abc-defg-hij?hl=en"),
    ("abc-defg-hij", "https://meet.google.com/abc-defg-hij?hl=en"),
    ("https://meet.google.com/abc-defg-hij?authuser=1&pli=1", "https://meet.google.com/abc-defg-hij?authuser=1&hl=en"),
    ("https://meet.google.com/lookup/team-sync", "https://meet.google.com/lookup/team-sync?hl=en"),
])
def test_a_meet_link_is_normalised(text, url):
    assert meet.parse_link(text) == (url, "")


@pytest.mark.parametrize("text", [
    "http://meet.google.com/abc-defg-hij",
    "https://meet.google.com.evil.example/abc-defg-hij",
    "https://evilmeet.google.com/abc-defg-hij",
    "https://zoom.us/j/123456",
    "javascript:alert(1)",
    "https://user@meet.google.com/abc-defg-hij",
    "https://meet.google.com:8443/abc-defg-hij",
    "https://meet.google.com/",
    "https://meet.google.com/abc-defg-hij/extra",
    "standup",
    "abc-defg-hi",
    "",
])
def test_anything_else_is_refused_with_a_reason_link(text):
    url, why = meet.parse_link(text)
    assert url is None and why


def test_the_meeting_code_labels_a_link_link():
    assert meet.meeting_code("https://meet.google.com/abc-defg-hij?hl=en") == "abc-defg-hij"


# ---- G6: the caption merger ----------------------------------------------------------------------

def snap(t: float, *rows: tuple[int, str, str], self_id: int = -1) -> dict[str, Any]:
    return {"t": t * 1000, "rows": [{"id": i, "speaker": s, "text": x, "self": i == self_id} for i, s, x in rows]}


def texts(lines):
    return [(ln.speaker, ln.text) for ln in lines]


def test_merge_growth_scroll_revision_and_new_caption():
    assert meet.merge("we should", "we should ship it") == "we should ship it"
    # the window scrolled: the front is gone, more at the end
    assert meet.merge("one two three four five six", "four five six seven eight") == \
        "one two three four five six seven eight"
    # the last words revised as Meet heard more
    assert meet.merge("we should shift", "we should ship it today") == "we should ship it today"
    # shrank (a redraw): keep the fuller line
    assert meet.merge("one two three four five", "three four five") == "one two three four five"
    # a different sentence: a new utterance
    assert meet.merge("we should ship it", "what about the budget") is None
    # live, 2026-09-25: Meet rewrote the first word, then inserted one mid-sentence
    live = ["He will most likely ship on Friday.",
            "We will most likely ship on Friday. Um. Yeah, it's quite important that Sam gets this deck by the end of the week, um.",
            "We will most likely ship on Friday. Um. Yeah, it's. It's quite important that Sam gets this deck by the "
            "end of the week, um. Hopefully, this is quite coherent to you. Thank you."]
    assert meet.merge(live[0], live[1]) == live[1]
    assert meet.merge(live[1], live[2]) == live[2]


def test_a_growing_caption_is_one_line_not_many():
    m = meet.CaptionMerger()
    out = []
    for k, words in enumerate(["We", "We should", "We should ship", "We should ship on Friday."]):
        out += m.feed(snap(100 + k * 0.3, (1, "Alice", words)))
    out += m.feed(snap(103, ))
    assert texts(out) == [("Alice", "We should ship on Friday.")]


def test_a_non_prefix_replacement_commits_the_old_line():
    m = meet.CaptionMerger()
    out = m.feed(snap(1, (1, "Alice", "We should ship on Friday")))
    out += m.feed(snap(2, (1, "Alice", "What about the budget")))
    out += m.finish()
    assert texts(out) == [("Alice", "We should ship on Friday"), ("Alice", "What about the budget")]


def test_a_scrolling_monologue_is_stitched_into_paragraphs():
    words = [f"w{i}" for i in range(150)]
    m = meet.CaptionMerger()
    out = []
    for end in range(10, 151, 5):                       # a window of the last 20 words, growing by 5
        out += m.feed(snap(end / 5, (7, "Bob", " ".join(words[max(0, end - 20):end]))))
    out += m.finish()
    assert " ".join(ln.text for ln in out) == " ".join(words)
    assert all(ln.speaker == "Bob" for ln in out) and len(out) >= 2      # paragraphs, not one wall


def test_own_tile_is_never_transcribed_caption():
    m = meet.CaptionMerger()
    out = m.feed(snap(1, (1, "Alice", "hello"), (2, "You", "not me"), self_id=2))
    out += m.finish()
    assert texts(out) == [("Alice", "hello")]


def test_a_re_rendered_block_continues_the_same_line_caption():
    m = meet.CaptionMerger()
    out = m.feed(snap(1, (1, "Alice", "we should ship")))
    out += m.feed(snap(1.3, (2, "Alice", "we should ship on Friday")))   # the element was replaced
    out += m.finish()
    assert texts(out) == [("Alice", "we should ship on Friday")]


def test_two_speakers_interleave_caption():
    m = meet.CaptionMerger()
    out = m.feed(snap(1, (1, "Alice", "shall we start")))
    out += m.feed(snap(2, (1, "Alice", "shall we start"), (2, "Bob", "yes let's")))
    out += m.feed(snap(4, (2, "Bob", "yes let's go")))
    out += m.finish()
    assert texts(out) == [("Alice", "shall we start"), ("Bob", "yes let's go")]


def test_the_live_revisions_are_one_line_caption():
    m = meet.CaptionMerger()
    out = []
    for k, words in enumerate(["He will most likely ship on Friday.",
                               "We will most likely ship on Friday. Um. Yeah, it's quite important that Sam gets this deck",
                               "We will most likely ship on Friday. Um. Yeah, it's. It's quite important that Sam gets "
                               "this deck by the end of the week. Thank you."]):
        out += m.feed(snap(100 + k, (1, "Alex Chen", words)))
    out += m.finish()
    assert texts(out) == [("Alex Chen", "We will most likely ship on Friday. Um. Yeah, it's. It's quite "
                           "important that Sam gets this deck by the end of the week. Thank you.")]


def test_a_pause_writes_the_line_down_and_more_words_add_only_the_rest_caption():
    m = meet.CaptionMerger()
    out = m.feed(snap(1, (1, "Alice", "first thought")))
    out += m.tick(1 + meet.IDLE_COMMIT_SECS)
    assert texts(out) == [("Alice", "first thought")]
    out += m.feed(snap(10, (1, "Alice", "first thought and a second one")))
    out += m.finish()
    assert texts(out) == [("Alice", "first thought"), ("Alice", "and a second one")]


# ---- G5: one call --------------------------------------------------------------------------------

class Clock:
    def __init__(self) -> None:
        self.t = 0.0

    def __call__(self) -> float:
        return self.t

    async def sleep(self, secs: float) -> None:
        self.t += secs
        await asyncio.sleep(0)


class FakePage:
    """States in order; the last repeats. A state is a dict the page script would return, or a callable of
    (opts) → dict for states that depend on what was asked (leave)."""

    def __init__(self, states: list[Any], clock: Clock) -> None:
        self.states, self.clock = list(states), clock
        self.opened: list[str] = []
        self.calls: list[dict[str, Any]] = []
        self.closed = False
        self.i = 0

    async def open(self, url: str) -> None:
        self.opened.append(url)

    async def run(self, opts: dict[str, Any]) -> dict[str, Any]:
        self.calls.append(dict(opts))
        st = self.states[min(self.i, len(self.states) - 1)]
        self.i += 1
        st = st(opts) if callable(st) else st
        if isinstance(st, Exception):
            raise st
        return dict(st)

    async def close(self) -> None:
        self.closed = True


PREJOIN = {"inCall": False, "micOn": False, "camOn": False}
IN_CALL = {"inCall": True, "captionsOn": True, "snaps": []}


ABOUTS: list[str] = []                     # each text's history line (meet._tell's about), newest last


def session(tmp_path: Path, states, **kw):
    clock = Clock()
    told: list[str] = []

    async def notify(text: str, about: str = "") -> None:
        told.append(text)
        ABOUTS.append(about)

    page = FakePage(states, clock)
    s = meet.MeetSession("https://meet.google.com/abc-defg-hij?hl=en", "Standup", page, tmp_path / "meetings",
                         notify, clock=clock, sleep=clock.sleep, epoch=lambda: 1_000_000 + clock.t,
                         wall=lambda: datetime(2026, 9, 26, 19, 0), **kw)
    return s, page, told, clock


def test_session_joins_listens_and_writes_notes_when_the_call_ends(tmp_path):
    summaries: list[str] = []

    def summarize(transcript: str) -> dict[str, Any]:
        summaries.append(transcript)
        return {"title": "Ship date", "gist": "Picked Friday.", "points": [], "decisions": ["Ship Friday"],
                "actions": ["Bob writes the notes"], "questions": [], "unclear": []}

    t0 = 1_000_000
    states = [PREJOIN, PREJOIN, IN_CALL,
              {**IN_CALL, "snaps": [{"seq": 1, "t": (t0 + 5) * 1000, "rows": [{"id": 1, "speaker": "Alice", "text": "Ship Friday?", "self": False}]}]},
              {**IN_CALL, "snaps": [{"seq": 2, "t": (t0 + 7) * 1000, "rows": [{"id": 2, "speaker": "Bob", "text": "Yes, Friday.", "self": False}]}]},
              {**IN_CALL, "snaps": []},
              {"inCall": False, "ended": "call ended"}]
    s, page, told, _ = session(tmp_path, states, summarize=summarize)
    state = asyncio.run(s.run())
    assert page.opened == ["https://meet.google.com/abc-defg-hij?hl=en"] and page.closed
    assert state.ended == "Meet said: call ended" and state.lines == 2
    assert told[0].startswith("I'm in Standup") and "microphone and camera off" in told[0]
    assert "Ship date" in told[-1] and "Ship Friday" in told[-1] and "Bob writes the notes" in told[-1]
    assert summaries == ["Alice: Ship Friday?\nBob: Yes, Friday."]
    body = state.notes.read_text()
    assert state.notes.parent == tmp_path / "meetings" / "2026-09-26"
    assert "# Ship date" in body and "**Alice**" in body and "**Bob**" in body and "Ship Friday" in body
    assert oct(state.notes.stat().st_mode & 0o777) == "0o600"
    # every poll before the call asks for no captions; every poll in it asks for them, with the cursor
    assert all(c.get("act") for c in page.calls)
    assert [c.get("since") for c in page.calls if c.get("captions")] == [0, 1, 2, 2]


def test_session_lobby_tells_the_owner_once_and_waits(tmp_path):
    lobby = {"inCall": False, "lobby": True}
    s, _, told, clock = session(tmp_path, [PREJOIN] + [lobby] * 100 + [IN_CALL, IN_CALL, {"inCall": False, "ended": "call ended"}])
    asyncio.run(s.run())
    assert sum("waiting to be let into Standup" in t for t in told) == 1
    assert any(t.startswith("I'm in Standup") for t in told)            # admitted after 100 s in the lobby


def test_session_lobby_gives_up_after_the_limit(tmp_path):
    left: list[bool] = []

    def lobby(opts):
        if opts.get("leave"):
            left.append(True)
            return {"inCall": False}
        return {"inCall": False, "lobby": True}

    s, page, told, clock = session(tmp_path, [lobby], config=meet.MeetConfig(lobby_minutes=2))
    state = asyncio.run(s.run())
    assert "nobody let me in within 2 minutes" in state.ended and left
    assert told[-1].startswith("I couldn't join Standup: nobody let me in") and page.closed
    # v3 safety review: the invite's title is outside words; the history keeps code's own line
    assert ABOUTS[-1] and "Standup" not in ABOUTS[-1]
    assert 120 <= clock.t < 125


@pytest.mark.parametrize("st,why", [
    ({"inCall": False, "ended": "no one responded to your request"}, "Meet said: no one responded"),
    ({"inCall": False, "ended": "you can't join this call"}, "Meet said: you can't join"),
    ({"inCall": False, "signIn": True}, "signed out"),
    ({"closed": True}, "the tab was closed"),
])
def test_session_ends_with_a_reason_the_owner_is_told(tmp_path, st, why):
    s, page, told, _ = session(tmp_path, [PREJOIN, st])
    state = asyncio.run(s.run())
    assert why in state.ended and why in told[-1] and page.closed
    assert state.notes is None                                          # never in: no notes file


def test_session_join_timeout_names_why(tmp_path):
    s, _, told, clock = session(tmp_path, [{"inCall": False, "switchHere": True, "joinHereToo": False}])
    state = asyncio.run(s.run())
    assert "won't take it from you" in state.ended and clock.t >= meet.JOIN_SECS
    s, _, told, _ = session(tmp_path, [{"inCall": False, "micOn": True}])
    assert "couldn't turn the microphone and camera off" in asyncio.run(s.run()).ended


def test_session_no_controls_is_allowed_only_after_a_while(tmp_path):
    s, page, _, _ = session(tmp_path, [PREJOIN] * 30 + [IN_CALL, {"inCall": False, "ended": "call ended"}])
    asyncio.run(s.run())
    flags = [(c["noControlsOk"]) for c in page.calls if "noControlsOk" in c]
    assert flags[0] is False and flags[int(meet.NO_CONTROLS_SECS) + 1] is True


def test_session_leave_presses_leave_and_writes_notes(tmp_path):
    leaves: list[int] = []

    def in_call(opts):
        if opts.get("leave"):
            leaves.append(1)
            return {"inCall": len(leaves) < 2}
        return IN_CALL

    s, page, told, clock = session(tmp_path, [PREJOIN, in_call])

    async def go():
        task = asyncio.create_task(s.run())
        while s.state.phase != "in call":
            await asyncio.sleep(0)
        s.leave()
        return await task

    state = asyncio.run(go())
    assert state.ended == "you asked me to leave" and len(leaves) == 2 and page.closed
    assert "Nothing was said" in told[-1]                               # an empty call: no model call
    assert state.notes.exists() and "nothing was said" in state.notes.read_text()


def test_session_redraw_is_not_the_end(tmp_path):
    out = {"inCall": False}
    s, _, _, _ = session(tmp_path, [PREJOIN, IN_CALL, out, IN_CALL, out, out, out])
    state = asyncio.run(s.run())
    assert state.ended == "the call ended" and state.joined_at is not None


def test_session_captions_missing_is_told_once(tmp_path):
    no_caps = {"inCall": True, "captionsOn": False, "snaps": []}
    s, _, told, _ = session(tmp_path, [PREJOIN] + [no_caps] * 80 + [{"inCall": False, "ended": "call ended"}])
    asyncio.run(s.run())
    assert sum("Captions won't turn on" in t for t in told) == 1


def test_every_text_naming_the_meeting_leaves_the_history_code_s_own_line(tmp_path):
    """Round-4 review: "I'm in {label}", the lobby and the captions texts went into the history as eve's own words,
    and the label can be a calendar invite's title. Each now carries an ``about`` without the label."""
    ABOUTS.clear()
    lobby = {"inCall": False, "lobby": True}
    no_caps = {"inCall": True, "captionsOn": False, "snaps": []}
    s, _, told, _ = session(tmp_path, [PREJOIN, lobby, lobby] + [no_caps] * 80 + [{"inCall": False, "ended": "x"}])
    asyncio.run(s.run())
    pairs = list(zip(told, ABOUTS, strict=True))
    named = [(t, a) for t, a in pairs if "Standup" in t]
    assert len(named) >= 4                               # the lobby, in, captions, the notes
    for text, about in named:
        assert about and "Standup" not in about, text


def test_session_a_page_that_stops_answering_ends_it(tmp_path):
    s, page, told, _ = session(tmp_path, [PREJOIN, IN_CALL] + [RuntimeError("target closed")] * 10)
    state = asyncio.run(s.run())
    assert "stopped answering" in state.ended and page.closed


def test_session_the_transcript_is_on_disk_before_the_end(tmp_path):
    t0 = 1_000_000
    seen: list[str] = []

    def summarize(transcript: str) -> dict[str, Any]:
        raise RuntimeError("model down")

    def later(opts):
        seen.append(s.state.notes.read_text() if s.state.notes else "")
        return {"inCall": False, "ended": "call ended"}

    states = [PREJOIN, IN_CALL, {**IN_CALL, "snaps": [{"seq": 1, "t": (t0 + 1) * 1000, "rows": [{"id": 1, "speaker": "Alice", "text": "Budget first.", "self": False}]}]},
              {**IN_CALL, "snaps": []}, {**IN_CALL, "snaps": []}, {**IN_CALL, "snaps": []}, later]
    s, _, told, _ = session(tmp_path, states, summarize=summarize)
    state = asyncio.run(s.run())
    assert "Budget first." in seen[0]                                   # written while the call was on
    assert "write-up failed" in told[-1] and "Budget first." in state.notes.read_text()


def test_notes_two_calls_in_one_minute_get_two_files(tmp_path):
    a = meet.NotesFile(tmp_path / "meetings", datetime(2026, 9, 26, 19, 0), "abc-defg-hij")
    b = meet.NotesFile(tmp_path / "meetings", datetime(2026, 9, 26, 19, 0), "abc-defg-hij")
    assert a.path != b.path and a.path.name == "1900-meet-abc-defg-hij.md" and b.path.name.endswith("-2.md")


# ---- G7: the calendar ----------------------------------------------------------------------------

PT = timezone(timedelta(hours=-7))


def ev(title: str, h: int, m: int = 0, mins: int = 30, link: str = "https://meet.google.com/aaa-bbbb-ccc",
       **extra) -> dict[str, Any]:
    start = datetime(2026, 9, 26, h, m, tzinfo=PT)
    e = {"summary": title, "start": {"dateTime": start.isoformat()},
         "end": {"dateTime": (start + timedelta(minutes=mins)).isoformat()}, **extra}
    if link:
        e["hangoutLink"] = link
    return e


EVENTS = [ev("Standup", 9, link="https://meet.google.com/sta-ndup-aaa"),
          ev("Lunch", 12, link=""),
          ev("Design review", 15, link="https://meet.google.com/des-ignr-evw"),
          ev("Test meeting", 19, link="https://meet.google.com/tes-tmee-tng"),
          {"summary": "Holiday", "start": {"date": "2026-09-26"}, "end": {"date": "2026-09-27"}}]


@pytest.mark.parametrize("words,now,title", [
    ("now", (9, 10), "Standup"),
    ("", (18, 55), "Test meeting"),                  # starts within the hour
    ("next", (12, 30), "Design review"),
    ("3pm", (8, 0), "Design review"),
    ("my 3", (8, 0), "Design review"),
    ("7 pm", (8, 0), "Test meeting"),
    ("19:00", (8, 0), "Test meeting"),
    ("the design review", (8, 0), "Design review"),
])
def test_calendar_finds_the_meeting(words, now, title):
    e, link, why = meet.find_meeting(EVENTS, words, datetime(2026, 9, 26, *now, tzinfo=PT))
    assert e is not None and e["summary"] == title and link.startswith("https://meet.google.com/") and not why


@pytest.mark.parametrize("words,now", [("now", (12, 10)), ("noon", (8, 0)), ("lunch", (8, 0)), ("11am", (8, 0))])
def test_calendar_no_match_is_a_plain_reason(words, now):
    e, link, why = meet.find_meeting(EVENTS, words, datetime(2026, 9, 26, *now, tzinfo=PT))
    assert e is None and link == "" and why


def test_calendar_reads_the_link_from_conference_data_and_nested_replies():
    e = ev("Sync", 10, link="", conferenceData={"entryPoints": [
        {"entryPointType": "phone", "uri": "tel:+1-555"},
        {"entryPointType": "video", "uri": "https://meet.google.com/syn-csyn-cxx"}]})
    reply = {"successful": True, "data": {"response_data": json.dumps({"items": [e]})}}
    found = meet.events_in(reply)
    assert found == [e]
    assert meet.event_link(e) == "https://meet.google.com/syn-csyn-cxx?hl=en"


def test_calendar_via_the_meeter_resolves_and_joins(tmp_path):
    clock = Clock()
    pages: list[FakePage] = []

    def make_page():
        pages.append(FakePage([PREJOIN, {"inCall": False, "ended": "call ended"}], clock))
        return pages[-1]

    asked: list[tuple[datetime, datetime]] = []

    def list_events(start, end):
        asked.append((start, end))
        return {"data": {"items": EVENTS}}

    async def go():
        m = meet.Meeter(meet.MeetConfig(), make_page, tmp_path, list_events=list_events, clock=clock,
                        sleep=clock.sleep, wall=lambda: datetime(2026, 9, 26, 14, 50, tzinfo=PT))
        r = await m.join("3pm")
        assert r["ok"] and r["joining"] == "Design review" and "des-ignr-evw" in r["link"]
        again = await m.join("https://meet.google.com/abc-defg-hij")
        assert not again["ok"] and "already in Design review" in again["reason"]
        await m.task
        return m

    m = asyncio.run(go())
    assert asked and asked[0][1] - asked[0][0] == timedelta(days=1)
    assert pages[0].opened == ["https://meet.google.com/des-ignr-evw?hl=en"]
    assert not m.busy and "not in a meeting" in m.status_line()


def test_calendar_meeter_without_a_calendar_asks_for_the_link(tmp_path):
    m = meet.Meeter(meet.MeetConfig(), lambda: None, tmp_path)
    r = asyncio.run(m.join("my 3pm"))
    assert not r["ok"] and "link" in r["reason"]
    r = asyncio.run(m.join("http://meet.google.com/abc-defg-hij"))
    assert not r["ok"] and "https" in r["reason"]


def test_meeter_tools_and_off_switch(tmp_path):
    m = meet.Meeter(meet.MeetConfig(enabled=False), lambda: None, tmp_path)
    assert [t["name"] for t in m.tools()] == ["meet_join", "meet_status", "meet_leave"]
    assert not asyncio.run(m.join("abc-defg-hij"))["ok"]
    assert asyncio.run(m.handle("meet_leave", {})) == {"ok": False, "reason": "I'm not in a meeting"}
    assert meet.configured({"EVE_MEET": "0"}).enabled is False
    cfg = meet.configured({"EVE_MEET_PROFILE": "Eve@Example.com", "EVE_MEET_LOBBY_MINUTES": "999"})
    assert cfg.profile == "eve@example.com" and cfg.lobby_minutes == 120 and cfg.quiet
