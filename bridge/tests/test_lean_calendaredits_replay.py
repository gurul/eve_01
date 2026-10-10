"""Replays of the counterexamples in verification/Eve/CalendarEdits.lean, against the real GoogleRestClient.

Every HTTP exchange goes through httpx.MockTransport, so the real request code runs and nothing leaves the Mac.
Each test failed on a35b043 (the code the model's ``current_violates`` describes).
"""
from __future__ import annotations

import os
import time
from datetime import datetime
from typing import Any, Callable
from zoneinfo import ZoneInfo

import httpx
import pytest

from eve_bridge import google_rest


def google(routes: Callable[[httpx.Request], Any]) -> google_rest.GoogleRestClient:
    def respond(request: httpx.Request) -> httpx.Response:
        out = routes(request)
        return out if isinstance(out, httpx.Response) else httpx.Response(200, json=out)

    return google_rest.GoogleRestClient("calendar", lambda: "t",
                                        http=httpx.Client(transport=httpx.MockTransport(respond)))


def test_list_events_reads_the_primary_when_the_calendar_list_has_a_network_error() -> None:
    """CalendarEdits `current_violates` (P4): the calendar list's request dropped (an httpx error, not a
    GoogleError), the primary calendar answered, and list_events still failed. A GoogleError on the same list
    falls back to the primary; a dropped connection did not, so the call failed without reading any calendar."""
    def routes(r: httpx.Request) -> Any:
        if r.url.path.endswith("/calendarList"):
            raise httpx.ConnectError("connection reset", request=r)
        return {"timeZone": "America/Los_Angeles", "items": [
            {"id": "m1", "summary": "Dentist", "start": {"dateTime": "2026-10-05T15:00:00-07:00"}}]}

    out = google(routes).call_tool("list_events", {"startTime": "2026-10-05T08:00:00-07:00"})
    assert out["isError"] is False
    assert [e["summary"] for e in out["structuredContent"]["events"]] == ["Dentist"]


@pytest.fixture
def los_angeles() -> Any:
    old = os.environ.get("TZ")
    os.environ["TZ"] = "America/Los_Angeles"
    time.tzset()
    try:
        yield ZoneInfo("America/Los_Angeles")
    finally:
        if old is None:
            os.environ.pop("TZ", None)
        else:
            os.environ["TZ"] = old
        time.tzset()


def test_suggest_time_keeps_to_working_hours_across_the_end_of_daylight_time(los_angeles: Any) -> None:
    """CalendarEdits `current_violates` (P5): free_slots built every day's 9-to-18 window in the start's fixed
    UTC offset. Asked from Friday 2026-10-30 (PDT, -07:00) to Wednesday 2026-11-04, the days after DST ends
    (2026-11-01) got 08:00-17:00 PST: an hour before the owner's working day was offered as free."""
    def routes(r: httpx.Request) -> Any:
        if r.url.path.endswith("/calendarList"):
            return {"items": [{"id": "me@x", "primary": True}]}
        return {"calendars": {"me@x": {"busy": []}}}

    out = google(routes).call_tool("suggest_time", {"startTime": "2026-10-30T00:00:00",
                                                    "endTime": "2026-11-04T00:00:00", "durationMinutes": 60})
    assert out["isError"] is False
    slots = out["structuredContent"]["free"]
    assert slots
    for s in slots:
        lo = datetime.fromisoformat(s["start"]).astimezone(los_angeles)
        hi = datetime.fromisoformat(s["end"]).astimezone(los_angeles)
        assert lo.date() == hi.date() and 9 <= lo.hour and (hi.hour, hi.minute) <= (18, 0), (s, lo, hi)


def test_suggest_time_keeps_to_the_owners_working_hours_when_asked_in_utc(los_angeles: Any) -> None:
    """CalendarEdits `current_violates` (P5, second trace): the same 9-to-18 window is built in whatever offset
    startTime carries, not in the owner's zone, and no clock change is needed. Asked for 2026-10-06 in UTC
    ("...Z", which the tool schema allows), the slot offered was 09:00-18:00 UTC, which is 02:00-11:00 in Los
    Angeles: the owner's night and early morning were offered as working time."""
    def routes(r: httpx.Request) -> Any:
        if r.url.path.endswith("/calendarList"):
            return {"items": [{"id": "me@x", "primary": True}]}
        return {"calendars": {"me@x": {"busy": []}}}

    out = google(routes).call_tool("suggest_time", {"startTime": "2026-10-06T00:00:00Z",
                                                    "endTime": "2026-10-07T00:00:00Z", "durationMinutes": 60})
    assert out["isError"] is False
    slots = out["structuredContent"]["free"]
    assert slots
    for s in slots:
        lo = datetime.fromisoformat(s["start"]).astimezone(los_angeles)
        hi = datetime.fromisoformat(s["end"]).astimezone(los_angeles)
        assert lo.date() == hi.date() and 9 <= lo.hour and (hi.hour, hi.minute) <= (18, 0), (s, lo, hi)
