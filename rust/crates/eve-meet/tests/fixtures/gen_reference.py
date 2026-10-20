"""Record meet.py's own answers for edge inputs, for the Rust port's differential test.

    PYTHONPATH=bridge/src python3 rust/crates/eve-meet/tests/fixtures/gen_reference.py > rust/crates/eve-meet/tests/fixtures/python_reference.json

Run from the repository root. Times that depend on the clock are recorded as epoch seconds and offsets, so the
Rust side rebuilds the same aware datetimes."""
import json
import re
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path

from eve_bridge import meet

LINKS = [
    "https://meet.google.com/abc-defg-hij", "meet.google.com/abc-defg-hij", "<https://meet.google.com/ABC-DEFG-HIJ/>",
    "abc-defg-hij", "(abc-defg-hij).", "https://meet.google.com/abc-defg-hij?authuser=1&pli=1",
    "https://meet.google.com/abc-defg-hij?authuser=a@b.com&authuser=2", "https://meet.google.com/abc-defg-hij?authuser=",
    "https://meet.google.com/abc-defg-hij?authuser=a%20b", "https://meet.google.com/abc-defg-hij#x",
    "https://meet.google.com/lookup/team-sync", "https://meet.google.com/lookup/ab", "https://meet.google.com/LOOKUP/Team_Sync",
    "http://meet.google.com/abc-defg-hij", "HTTPS://MEET.GOOGLE.COM/abc-defg-hij", "https://meet.google.com.evil.example/abc-defg-hij",
    "https://evilmeet.google.com/abc-defg-hij", "https://zoom.us/j/123456", "javascript:alert(1)",
    "https://user@meet.google.com/abc-defg-hij", "https://@meet.google.com/abc-defg-hij", "https://:@meet.google.com/abc-defg-hij",
    "https://u:p@meet.google.com/abc-defg-hij", "https://meet.google.com:8443/abc-defg-hij", "https://meet.google.com:443/abc-defg-hij",
    "https://meet.google.com:/abc-defg-hij", "https://meet.google.com/", "https://meet.google.com/abc-defg-hij/extra",
    "standup", "abc-defg-hi", "", "   ", "join at https://zoom.us/x then meet.google.com/abc-defg-hij",
    "ftp://meet.google.com/abc-defg-hij", "https://[::1]/abc-defg-hij", "https://meet.google.com]/abc-defg-hij",
    "see https://meet.google.com/xyz-abcd-efg, thanks!", "https://meet.google.com//abc-defg-hij//",
    "wss://meet.google.com/abc-defg-hij", "https://meet.google.com/abc-defg-hij?hl=fr&authuser=x+y",
]
MERGES = [
    ("we should", "we should ship it"), ("one two three four five six", "four five six seven eight"),
    ("we should shift", "we should ship it today"), ("one two three four five", "three four five"),
    ("we should ship it", "what about the budget"), ("", "hello"), ("hello", ""), ("a", "a"), ("a b", "b c"),
    ("Hello, world.", "hello world again"), ("x y z", "y"), ("the the the", "the the"), ("a b c d e", "c d x y z"),
    ("one two three", "zero one two three four"), ("it's quite", "it's. It's quite important"),
    ("a b c d e f g h", "e f g h i j k l m n o p"), ("Ship Friday?", "Yes, Friday."),
]


def snap(t, *rows, self_id=-1):
    return {"t": t * 1000, "rows": [{"id": i, "speaker": s, "text": x, "self": i == self_id} for i, s, x in rows]}


SCRIPTS = [
    [("feed", snap(100, (1, "Alice", "We"))), ("feed", snap(100.3, (1, "Alice", "We should"))),
     ("feed", snap(100.6, (2, "Alice", "We should ship"))), ("feed", snap(103))],
    [("feed", snap(1, (1, "Alice", "first thought"))), ("tick", 5.0), ("feed", snap(10, (1, "Alice", "first thought and a second one"))),
     ("finish", None)],
    [("feed", snap(1, (1, "Alice", "shall we start"))), ("feed", snap(2, (1, "Alice", "shall we start"), (2, "Bob", "yes let's"))),
     ("feed", snap(4, (2, "Bob", "yes let's go"))), ("finish", None)],
    [("feed", snap(1, (1, "Alice", "hello there"))), ("feed", snap(1.5, (1, "Bob", "hello there friend"))), ("finish", None)],
    [("feed", {"t": 5000, "rows": [{"id": "3", "speaker": "  Al   ice ", "text": " a  b "}, {"id": None, "text": "x"},
                                   {"id": "z", "text": "y"}, "row", {"id": 4.7, "speaker": 7, "text": "num"}]}), ("finish", None)],
    [("feed", snap(1, (1, "Bob", " ".join(f"w{i}" for i in range(70))))), ("feed", snap(2, (1, "Bob", " ".join(f"w{i}" for i in range(140))))),
     ("tick", 2.5), ("finish", None)],
    [("feed", snap(1, (1, "A", "one two three"), (2, "B", "four five six"))), ("feed", snap(3, (3, "A", "one two three seven"))),
     ("feed", snap(3.2, (3, "A", "one two three seven"), (4, "B", "four five six eight"))), ("finish", None)],
]


def run_script(steps):
    m = meet.CaptionMerger()
    out = []
    for op, arg in steps:
        if op == "feed":
            lines = m.feed(arg)
        elif op == "tick":
            lines = m.tick(arg)
        else:
            lines = m.finish()
        out.append([[ln.speaker, ln.text, ln.at] for ln in lines])
    return out


PT = timezone(timedelta(hours=-7))


def ev(title, h, m=0, mins=30, link="https://meet.google.com/aaa-bbbb-ccc", **extra):
    start = datetime(2026, 9, 26, h, m, tzinfo=PT)
    e = {"summary": title, "start": {"dateTime": start.isoformat()},
         "end": {"dateTime": (start + timedelta(minutes=mins)).isoformat()}, **extra}
    if link:
        e["hangoutLink"] = link
    return e


EVENTS = [ev("Standup", 9, link="https://meet.google.com/sta-ndup-aaa"), ev("Lunch", 12, link=""),
          ev("Design review", 15, link="https://meet.google.com/des-ignr-evw"),
          ev("Test meeting", 19, link="https://meet.google.com/tes-tmee-tng"),
          ev("Cancelled one", 16, link="https://meet.google.com/can-celd-one", status="cancelled"),
          {"summary": "UTC sync", "start": {"dateTime": "2026-09-26T23:00:00Z"}, "hangoutLink": "https://meet.google.com/utc-sync-aaa"},
          {"summary": "Holiday", "start": {"date": "2026-09-26"}, "end": {"date": "2026-09-27"}},
          {"summary": "Desc only", "start": {"dateTime": "2026-09-26T17:30:00-07:00"},
           "description": "dial in: https://meet.google.com/des-cron-lyy"},
          {"summary": "Video", "start": {"dateTime": "2026-09-26T18:15:00.500-07:00"}, "conferenceData": {"entryPoints": [
              {"entryPointType": "phone", "uri": "tel:1"}, {"entryPointType": "video", "uri": "https://meet.google.com/vid-eovi-deo"}]}}]
FINDS = [("now", (9, 10)), ("", (18, 55)), ("next", (12, 30)), ("3pm", (8, 0)), ("my 3", (8, 0)), ("7 pm", (8, 0)),
         ("19:00", (8, 0)), ("the design review", (8, 0)), ("join at 9 p.m.", (8, 0)), ("9 A.M.", (8, 0)), ("noon", (8, 0)),
         ("lunch", (8, 0)), ("11am", (8, 0)), ("now", (12, 10)), ("25:00", (8, 0)), ("12am", (8, 0)), ("12pm", (8, 0)),
         ("at 4", (8, 0)), ("next", (22, 0)), ("join my meeting right now", (17, 25)), ("video", (8, 0)), ("4:30", (8, 0)),
         ("at 9:61", (8, 0)), ("the sync", (8, 0)), ("standup please", (8, 0)), ("in progress", (19, 10)), ("my 8", (8, 0)),
         ("current call", (6, 0)), ("NEXT one", (8, 0)), ("6:15 pm", (8, 0)), ("meeting at 5 30", (8, 0))]


def find(words, now):
    e, link, why = meet.find_meeting(EVENTS, words, datetime(2026, 9, 26, *now, tzinfo=PT))
    return [e.get("summary") if e else None, link, why]


SUMMARIES = [
    {"title": "Ship date", "gist": "Picked Friday.", "points": ["a", " b  c ", "", 3, True, None, ["x"]], "decisions": ["Ship Friday"],
     "actions": ["Bob writes the notes"], "questions": [], "unclear": ["the last bit"]},
    {}, {"title": "Standup."}, {"title": "", "gist": "  ", "decisions": "not a list", "actions": [str(i) for i in range(12)]},
    {"title": 5, "gist": 1.5},
]


def notes_case(summary, lines, label):
    with tempfile.TemporaryDirectory() as tmp:
        f = meet.NotesFile(Path(tmp) / "meetings", datetime(2026, 9, 26, 19, 0), "abc-defg-hij")
        f.append([meet.Line(s, t, at) for s, t, at in lines])
        f.finish(summary, label, "the call ended")
        body = re.sub(r"^ended: \d{4}-\d\d-\d\d \d\d:\d\d ", "ended: <now> ", f.path.read_text(), flags=re.M)
        return {"summary": summary, "lines": lines, "label": label, "name": f.path.name, "body": body,
                "message": meet.summary_message(label, "the call ended", summary, len(f.lines), f.path)}


LINES = [["Alice", "Ship Friday?", 1_000_005.0], ["", "Yes.", 0.0], ["Bob", "Done.", 1_700_000_000.25]]

CONFIGS = [{}, {"EVE_MEET": "0"}, {"EVE_MEET": " On "}, {"EVE_MEET": ""}, {"EVE_MEET_PROFILE": " Eve@Example.com "},
           {"EVE_MEET_LOBBY_MINUTES": "999"}, {"EVE_MEET_LOBBY_MINUTES": "0.5"}, {"EVE_MEET_LOBBY_MINUTES": "x"},
           {"EVE_MEET_LOBBY_MINUTES": "nan"}, {"EVE_MEET_MAX_HOURS": "inf"}, {"EVE_MEET_MAX_HOURS": " 1_5 "},
           {"EVE_MEET_QUIET": "off"}, {"EVE_MEET_QUIET": "nope"}, {"EVE_MEET_GUEST_NAME": "  " + "N" * 50 + "  "},
           {"EVE_MEET_GUEST_NAME": "   "}]


def main():
    out = {
        "links": [[t, *meet.parse_link(t)] for t in LINKS],
        "codes": [[u, meet.meeting_code(u)] for u in ["https://meet.google.com/abc-defg-hij?hl=en",
                                                      "https://meet.google.com/lookup/team?hl=en", "https://meet.google.com/", ""]],
        "merges": [[a, b, meet.merge(a, b)] for a, b in MERGES],
        "scripts": [[steps, run_script(steps)] for steps in SCRIPTS],
        "events": EVENTS,
        "event_links": [meet.event_link(e) for e in EVENTS],
        "events_in": [[x, len(meet.events_in(x))] for x in [{"data": {"response_data": json.dumps({"items": EVENTS})}},
                                                         [[[[[[[[EVENTS[0]]]]]]]]], "  " + json.dumps(EVENTS), {"x": "{bad"}]],
        "finds": [[w, list(now), find(w, now)] for w, now in FINDS],
        "notes": [notes_case(s, LINES, "Standup") for s in SUMMARIES] + [notes_case({}, [], "Call x")],
        "configs": [[env, meet.dataclasses.asdict(meet.configured(env))] for env in CONFIGS],
        "line_texts": [[s, t, at, meet.line_text(meet.Line(s, t, at))] for s, t, at in LINES],
        "tools": meet.TOOLS, "tool_names": list(meet.TOOL_NAMES), "instructions": meet.INSTRUCTIONS_BLOCK,
        "prompt": meet.SUMMARY_PROMPT, "schema": meet.MEET_SCHEMA, "off_reason": meet.OFF_REASON,
        "abouts": [meet.ABOUT_IN, meet.ABOUT_LOBBY, meet.ABOUT_CAPTIONS, meet.ABOUT_JOINING, meet.ABOUT_NOT_JOINING],
        "why_not_in": [[st, meet.MeetSession._why_not_in(st)] for st in [
            {"switchHere": True}, {"switchHere": True, "joinHereToo": True}, {"permission": 1}, {"camOn": True}, {}]],
        "g": [[x, f"{x:g}"] for x in [0.25, 2.0, 15.0, 120.0, 1.5, 0.1, 7.25, 1234567.0, 0.00001, 100000.0, 999999.5, 12.345678]],
    }
    print(json.dumps(out, indent=1))


main()
