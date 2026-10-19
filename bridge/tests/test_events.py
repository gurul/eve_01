"""events.py: Ticketmaster's Discovery search, with no network (the fetch is a fake)."""

from __future__ import annotations

import asyncio
import json
import time
import urllib.parse
from datetime import datetime, timedelta, timezone
from typing import Any
from zoneinfo import ZoneInfo

import pytest

from eve_bridge import events, toolset_router, watch

KEY = {"TICKETMASTER_API_KEY": "tm-test-key-value"}
PACIFIC = ZoneInfo("America/Los_Angeles")       # a real zone: the query follows its daylight saving
NOW = datetime(2026, 10, 8, 18, 30, tzinfo=PACIFIC)        # a Thursday evening


def _event(name: str, *, price: Any = None, genre: str = "Rock") -> dict[str, Any]:
    e: dict[str, Any] = {
        "name": name, "id": "x", "url": f"https://www.ticketmaster.com/{name.lower()}/event/0F00",
        "dates": {"start": {"localDate": "2026-10-10", "localTime": "19:30:00"}, "status": {"code": "onsale"}},
        "classifications": [{"segment": {"name": "Music"}, "genre": {"name": genre}}],
        "_embedded": {"venues": [{"name": "Neptune Theatre", "city": {"name": "Seattle"},
                                  "state": {"stateCode": "WA"}}]},
        "images": [{"url": "https://img.invalid/" + "x" * 500}], "sales": {"public": {}}, "info": "y" * 900,
    }
    if price is not None:
        e["priceRanges"] = [price]
    return e


def _payload(*evs: dict[str, Any], total: int = 42) -> str:
    return json.dumps({"_embedded": {"events": list(evs)}, "page": {"totalElements": total}})


class Fetch:
    def __init__(self, body: str = "", error: Exception | None = None, delay: float = 0.0) -> None:
        self.body, self.error, self.delay = body, error, delay
        self.urls: list[str] = []

    def __call__(self, url: str, **kw: Any) -> tuple[int, str]:
        self.urls.append(url)
        if self.delay:
            time.sleep(self.delay)
        if self.error is not None:
            raise self.error
        return 200, self.body

    def query(self) -> dict[str, str]:
        return dict(urllib.parse.parse_qsl(urllib.parse.urlsplit(self.urls[-1]).query))


def _finder(fetch: Fetch, **kw: Any) -> events.Events:
    kw.setdefault("where", lambda: None)            # never the real phone's fix
    return events.Events(fetch=fetch, now=lambda: NOW, environ=kw.pop("environ", KEY), **kw)


def _args(**over: Any) -> dict[str, Any]:
    base = {"city": "", "near": False, "keyword": "", "category": "", "start_date": "", "end_date": ""}
    base.update(over)
    return base


def test_a_weekend_in_a_city_becomes_the_discovery_query_in_utc() -> None:
    fetch = Fetch(_payload(_event("Band")))
    out = asyncio.run(_finder(fetch).search(_args(city="Seattle, wa", category="music", keyword="indie rock",
                                                  start_date="2026-10-10", end_date="2026-10-11")))
    assert out["ok"] and len(out["events"]) == 1
    q = fetch.query()
    assert fetch.urls[-1].startswith(watch.TM_API + "/events.json?")
    assert q["city"] == "Seattle" and q["stateCode"] == "WA" and q["classificationName"] == "music"
    assert q["keyword"] == "indie rock" and q["sort"] == "date,asc" and q["size"] == str(events.MAX_EVENTS)
    # Saturday 00:00 to Sunday 23:59:59 Pacific (UTC-7), in UTC with a Z and no fraction
    assert q["startDateTime"] == "2026-10-10T07:00:00Z" and q["endDateTime"] == "2026-10-12T06:59:59Z"


def test_today_starts_now_and_no_dates_means_from_now_with_no_end() -> None:
    q = events.query(_args(start_date="2026-10-08"), NOW)
    assert q["startDateTime"] == "2026-10-09T01:30:00Z" and "endDateTime" not in q
    q = events.query(_args(), NOW)
    assert q["startDateTime"] == "2026-10-09T01:30:00Z" and "city" not in q and "classificationName" not in q


@pytest.mark.parametrize("bad", [{"start_date": "next friday"}, {"start_date": "2026-10-12", "end_date": "2026-10-10"}])
def test_a_bad_date_is_a_reason_and_no_request(bad: dict[str, str]) -> None:
    fetch = Fetch(_payload())
    out = asyncio.run(_finder(fetch).search(_args(**bad)))
    assert out["ok"] is False and "YYYY-MM-DD" in out["reason"] and fetch.urls == []


def test_each_event_is_trimmed_to_what_a_reply_needs() -> None:
    body = _payload(_event("Band", price={"currency": "usd", "min": 35, "max": 89.5}),
                    _event("Other", genre="Undefined"), total=566)
    out = events.parse(json.loads(body))
    assert out["total"] == 566
    first, second = out["events"]
    assert first == {"name": "Band", "date": "2026-10-10", "time": "19:30", "venue": "Neptune Theatre",
                     "city": "Seattle, WA", "status": "onsale", "genre": "Rock",
                     "url": "https://www.ticketmaster.com/band/event/0F00", "price": "USD 35-89.50"}
    assert "price" not in second and second["genre"] == ""          # unknown, never free; "Undefined" is no genre
    assert len(json.dumps(first)) < 400                              # the raw event is kilobytes


def test_at_most_max_events_and_none_found_says_so() -> None:
    out = events.parse(json.loads(_payload(*[_event(f"B{i}") for i in range(12)])))
    assert len(out["events"]) == events.MAX_EVENTS
    empty = events.parse({"page": {"totalElements": 0}})
    assert empty["ok"] and empty["events"] == [] and "no events" in empty["note"]


def test_no_key_is_a_clear_reason_and_no_tool() -> None:
    fetch = Fetch(_payload())
    finder = _finder(fetch, environ={})
    assert finder.tools() == [] and not finder.enabled()
    out = asyncio.run(finder.search(_args(city="Seattle")))
    assert out == {"ok": False, "reason": events.OFF_REASON} and fetch.urls == []
    assert _finder(fetch).tools() == [events.TOOL]


def test_a_slow_answer_times_out_quickly_and_never_raises() -> None:
    fetch = Fetch(_payload(), delay=1.0)

    async def timed() -> tuple[dict[str, Any], float]:
        t0 = time.monotonic()
        out = await _finder(fetch, timeout_s=0.1).search(_args(city="Seattle"))
        return out, time.monotonic() - t0       # the turn's wait (the worker thread finishes on its own)

    out, waited = asyncio.run(timed())
    assert out == {"ok": False, "reason": "Ticketmaster did not answer in time"}
    assert waited < 0.9


@pytest.mark.parametrize("error,reason", [
    (watch.FetchError("the site answered HTTP 401", 401), "refused the key"),
    (watch.FetchError("the site answered HTTP 429", 429), "slow down"),
    (watch.FetchError("the site took too long to answer"), "did not answer in time"),
    (watch.FetchError("the site could not be reached"), "could not be reached"),
    (RuntimeError("boom " + KEY["TICKETMASTER_API_KEY"]), "could not be reached"),
])
def test_failures_are_reasons_by_status_and_never_carry_the_key(error: Exception, reason: str) -> None:
    out = asyncio.run(_finder(Fetch(error=error)).search(_args(city="Seattle")))
    assert out["ok"] is False and reason in out["reason"]
    assert KEY["TICKETMASTER_API_KEY"] not in json.dumps(out)


def test_an_unreadable_answer_is_a_reason() -> None:
    out = asyncio.run(_finder(Fetch("<html>nope</html>")).search(_args()))
    assert out == {"ok": False, "reason": "Ticketmaster sent an unreadable answer"}


def test_a_search_is_counted_by_the_watchers_limiter() -> None:
    taken: list[str] = []

    class Limiter:
        def take(self, host: str) -> None:
            taken.append(host)

    asyncio.run(_finder(Fetch(_payload()), limiter=lambda: Limiter()).search(_args()))
    assert taken == [watch.TM_HOST]


def test_the_tool_is_strict_and_sits_in_the_always_carried_events_toolset() -> None:
    params = events.TOOL["parameters"]
    assert events.TOOL["strict"] and set(params["required"]) == set(params["properties"])
    assert toolset_router.ALWAYS[toolset_router.EVENTS] == events.TOOL_NAMES
    assert toolset_router.always_toolset(events.TOOL_NAME) == toolset_router.EVENTS
    # its line names watch_add only when the watcher offers the ticketmaster kind
    assert "watch_add" not in events.instructions(False) and "watch_add" in events.instructions(True)


# ---- day boundaries across daylight saving (review, 2026-10-08) -------------------------------------------------

def test_a_day_after_the_clocks_change_ends_at_that_day_s_own_midnight() -> None:
    # 2026-11-01 is the end of daylight saving in Los Angeles: Monday 2026-11-02 ends at Tuesday 00:00 PST (UTC-8)
    now = datetime(2026, 10, 30, 12, 0, tzinfo=PACIFIC)
    q = events.query(_args(start_date="2026-11-02", end_date="2026-11-02"), now)
    assert q["startDateTime"] == "2026-11-02T08:00:00Z" and q["endDateTime"] == "2026-11-03T07:59:59Z"


def test_a_fixed_offset_now_reads_each_day_in_the_mac_s_own_zone(monkeypatch: pytest.MonkeyPatch) -> None:
    """datetime.now().astimezone() carries today's fixed offset (PDT, -7); a day after the change is PST all the
    same. Was 06:59:59Z (an hour off) before the fix."""
    monkeypatch.setenv("TZ", "America/Los_Angeles")
    time.tzset()
    try:
        now = datetime(2026, 10, 30, 12, 0).astimezone()
        assert isinstance(now.tzinfo, timezone) and now.utcoffset() == timedelta(hours=-7)
        q = events.query(_args(start_date="2026-11-02", end_date="2026-11-02"), now)
        assert q["startDateTime"] == "2026-11-02T08:00:00Z" and q["endDateTime"] == "2026-11-03T07:59:59Z"
    finally:
        monkeypatch.undo()
        time.tzset()


# ---- the watcher's limiter, honoured as the watches honour it ---------------------------------------------------

class _Limiter:
    def __init__(self, held: float = 0.0, wait: float = 0.0) -> None:
        self.held, self.wait = held, wait
        self.taken: list[str] = []
        self.penalized: list[tuple[str, Any]] = []
        self.cleared: list[str] = []

    def backed_off(self, host: str) -> float:
        return self.held

    def ready_in(self, host: str) -> float:
        return self.wait

    def take(self, host: str) -> None:
        self.taken.append(host)

    def penalize(self, host: str, retry_after: Any = None) -> float:
        self.penalized.append((host, retry_after))
        return 30.0

    def clear(self, host: str) -> None:
        self.cleared.append(host)


def test_a_back_off_ticketmaster_asked_for_is_kept_with_no_request() -> None:
    fetch, lim = Fetch(_payload()), _Limiter(held=42.0)
    out = asyncio.run(_finder(fetch, limiter=lambda: lim).search(_args()))
    assert out == {"ok": False, "reason": "Ticketmaster asked to slow down; try again in 42 s"}
    assert fetch.urls == [] and lim.taken == []


def test_the_watches_pacing_never_refuses_the_owner_s_search() -> None:
    """2026-10-18: a retry three seconds after a first search was refused by the watches' 20 s host gap."""
    fetch, lim = Fetch(_payload()), _Limiter(wait=17.0)
    started = time.monotonic()
    out = asyncio.run(_finder(fetch, limiter=lambda: lim).search(_args()))
    assert out["ok"] and len(fetch.urls) == 1 and time.monotonic() - started < 1.0
    assert lim.taken == [watch.TM_HOST] and lim.cleared == [watch.TM_HOST]


def test_the_real_limiter_lets_a_second_search_through_at_once() -> None:
    lim = watch.RateLimiter()
    fetch = Fetch(_payload(_event("A")))
    finder = _finder(fetch, limiter=lambda: lim)
    assert asyncio.run(finder.search(_args(city="Seattle")))["ok"]
    assert asyncio.run(finder.search(_args(city="Seattle")))["ok"] and len(fetch.urls) == 2


def test_a_429_backs_the_shared_key_off_for_the_watches_too() -> None:
    lim = _Limiter()
    error = watch.FetchError("the site answered HTTP 429", 429, retry_after=20.0)
    out = asyncio.run(_finder(Fetch(error=error), limiter=lambda: lim).search(_args()))
    assert "slow down" in out["reason"] and lim.penalized == [(watch.TM_HOST, 20.0)] and lim.cleared == []


# ---- near the owner (2026-10-18: "Concerts near me" searched everywhere) ----------------------------------------

HERE = (41.88, -87.63)


@pytest.mark.parametrize("over", [{"category": "music"}, {"near": True, "keyword": "jazz"}])
def test_near_me_searches_around_the_owner(over: dict[str, Any]) -> None:
    fetch = Fetch(_payload(_event("A")))
    out = asyncio.run(_finder(fetch, where=lambda: HERE).search(_args(**over)))
    q = fetch.query()
    assert q["latlong"] == "41.88,-87.63" and q["radius"] == str(events.NEAR_RADIUS_MI) and q["unit"] == "miles"
    assert "city" not in q and out["ok"] and "miles" in out["near"]


@pytest.mark.parametrize("over", [{"city": "Portland, OR", "near": True}, {"keyword": "Mariners"}])
def test_a_named_city_or_an_artist_s_tour_is_not_near(over: dict[str, Any]) -> None:
    asked: list[int] = []
    fetch = Fetch(_payload(_event("A")))
    out = asyncio.run(_finder(fetch, where=lambda: asked.append(1) or HERE).search(_args(**over)))
    assert "latlong" not in fetch.query() and "near" not in out and asked == []


def test_near_me_with_no_known_place_searches_everywhere_and_says_so() -> None:
    fetch = Fetch(_payload(_event("A")))
    out = asyncio.run(_finder(fetch).search(_args(near=True)))
    assert "latlong" not in fetch.query() and out["note"] == events.NO_HERE


def test_here_is_the_phone_s_fresh_fix_else_the_weather_place_rounded(tmp_path: Any) -> None:
    from eve_bridge import phone_location

    env = {"EVE_PHONE_LOCATION_FILE": str(tmp_path / "loc.json"), "EVE_WEATHER_LAT": "40.71277",
           "EVE_WEATHER_LON": "-74.00597"}
    assert events.here(env) == (40.71, -74.01)
    fix = phone_location.parse({"lat": 41.878114, "lon": -87.629798, "accuracy_m": 10,
                                "at": datetime.now(timezone.utc).isoformat(), "place": None, "speed_mps": None})
    assert phone_location.store(fix, tmp_path / "loc.json")
    assert events.here(env) == (41.88, -87.63)
    assert events.here({"EVE_PHONE_LOCATION_FILE": str(tmp_path / "none.json")}) is None
    assert events.here({**env, "EVE_PHONE_LOCATION": "0", "EVE_WEATHER_LAT": "x"}) is None
