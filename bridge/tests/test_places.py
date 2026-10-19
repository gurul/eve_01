"""places.py: the places history, habits, digests and things, with a lent clock and synthetic weeks (no network)."""

from __future__ import annotations

import asyncio
import os
from datetime import date, datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Optional
from zoneinfo import ZoneInfo

import pytest

from eve_bridge import phone_location, places, system_context

PT = ZoneInfo("America/Los_Angeles")
# made-up points in Chicago, none of them anyone's
HOME = (41.8800, -87.6300)
WORK = (41.8890, -87.6350)          # about 1 km north
GYM = (41.8700, -87.6400)
MARKET = (41.8950, -87.6200)
NOW = datetime(2026, 10, 22, 12, 0, tzinfo=PT)       # a Thursday noon


def t(d: date, hh: int, mm: int = 0) -> datetime:
    return datetime(d.year, d.month, d.day, hh, mm, tzinfo=PT).astimezone(timezone.utc)


def env(tmp_path: Path, **over: str) -> dict[str, str]:
    out = {"EVE_TIMEZONE": "America/Los_Angeles", "EVE_PLACES_FILE": str(tmp_path / "places.db"),
           "EVE_PHONE_LOCATION_FILE": str(tmp_path / "phone.json")}
    out.update(over)
    return out


def store(tmp_path: Path, now: datetime = NOW, **over: str) -> places.Places:
    return places.Places(environ=env(tmp_path, **over), now=lambda: now.astimezone(timezone.utc))


def visit(s: places.Places, where: tuple[float, float], a: datetime, b: Optional[datetime], *,
          poi: Optional[str] = None, category: Optional[str] = None, acc: float = 20.0) -> None:
    """As the phone sends it: the arrival, then the departure with the same arrival."""
    s.record_visit(places.Visit(where[0], where[1], acc, a, None, None, poi, category))
    if b is not None:
        s.record_visit(places.Visit(where[0], where[1], acc, a, b, None, poi, category))


def four_weeks(s: places.Places, *, skip_gym_on: Optional[date] = None) -> None:
    """Home nights, work 09:00-17:30 on weekdays, the gym Mon/Wed/Fri 18:00-19:00, a market one Saturday."""
    first = NOW.date() - timedelta(days=28)
    home_from = t(first - timedelta(days=1), 19, 15)
    d = first
    while d < NOW.date():
        wd = d.weekday()
        if wd < 5:
            visit(s, HOME, home_from, t(d, 8, 30))
            visit(s, WORK, t(d, 9), t(d, 17, 30))
            if wd in (0, 2, 4) and d != skip_gym_on:
                visit(s, GYM, t(d, 18), t(d, 19), poi="Equinox", category="fitnessCenter")
                home_from = t(d, 19, 15)
            else:
                home_from = t(d, 17, 50)
        elif wd == 5 and d == first + timedelta(days=(5 - first.weekday()) % 7):
            visit(s, HOME, home_from, t(d, 10, 40))
            visit(s, MARKET, t(d, 11), t(d, 12), poi="Green City Market")
            home_from = t(d, 12, 20)
        d += timedelta(days=1)
    visit(s, HOME, home_from, t(NOW.date(), 8, 30))
    visit(s, WORK, t(NOW.date(), 9), None)                  # at work now


def share_fix(e: dict[str, str], where: tuple[float, float], place: str = "a street") -> None:
    fix = phone_location.parse({"lat": where[0], "lon": where[1], "accuracy_m": 10.0,
                                "at": datetime.now(timezone.utc).isoformat(), "place": place, "speed_mps": None})
    path = Path(e["EVE_PHONE_LOCATION_FILE"])
    path.unlink(missing_ok=True)
    assert phone_location.store(fix, path)


# ---- visits and places -------------------------------------------------------------------------------------------

def test_an_arrival_and_its_departure_are_one_visit_at_a_named_place(tmp_path: Path) -> None:
    s = store(tmp_path)
    d = NOW.date()
    visit(s, GYM, t(d, 7), t(d, 8, 5), poi="Equinox", category="fitnessCenter")
    [v] = s.timeline(d, d)
    assert v["place"] == "Equinox" and v["kind"] == "fitness center"
    assert v["arrived"].endswith("07:00") and v["left"].endswith("08:05") and v["stayed"] == "1 h 5 min"
    assert "approximate" not in v and "lat" not in v and "lon" not in v      # words, not coordinates


def test_nearby_visits_share_a_place_and_a_far_one_starts_another(tmp_path: Path) -> None:
    s = store(tmp_path)
    d = NOW.date()
    visit(s, GYM, t(d, 6), t(d, 7), poi="Equinox")
    visit(s, (GYM[0] + 0.0018, GYM[1]), t(d, 8), t(d, 9), poi="Equinox")     # 200 m off, the same point of interest
    visit(s, (GYM[0] + 0.0006, GYM[1]), t(d, 10), t(d, 11))                  # 70 m off, no name
    visit(s, MARKET, t(d, 11, 30), t(d, 11, 45))
    assert s.summary()["places"] == 2 and s.summary()["visits"] == 4


def test_a_stay_is_read_from_fixes_and_an_ios_visit_replaces_it(tmp_path: Path) -> None:
    s = store(tmp_path)
    d = NOW.date()

    def fix(where: tuple[float, float], at: datetime, acc: float = 15.0) -> Any:
        return s.record_fix(phone_location.Fix(where[0], where[1], acc, at, "Main St"))

    assert fix(MARKET, t(d, 9)) is None
    assert fix((MARKET[0] + 0.0003, MARKET[1]), t(d, 9, 5)) is None
    assert fix(MARKET, t(d, 9, 20)) is None
    assert fix(HOME, t(d, 9, 40)) is not None                   # 1.7 km away: the stay ended before this fix
    [v] = s.timeline(d, d)
    assert v["approximate"] is True and v["arrived"].endswith("09:00")
    left = int(v["left"][-5:-3]) * 60 + int(v["left"][-2:])
    assert 9 * 60 + 20 <= left < 9 * 60 + 40                     # the far fix minus the travel time
    visit(s, MARKET, t(d, 8, 58), t(d, 9, 33), poi="Green City Market")
    [v] = s.timeline(d, d)
    assert v["place"] == "Green City Market" and "approximate" not in v


def test_a_short_stop_and_a_vague_fix_are_not_visits(tmp_path: Path) -> None:
    s = store(tmp_path)
    d = NOW.date()
    s.record_fix(phone_location.Fix(*MARKET, 15.0, t(d, 9)))
    s.record_fix(phone_location.Fix(*MARKET, 5000.0, t(d, 9, 30)))           # a cell-tower guess says nothing
    assert s.record_fix(phone_location.Fix(*HOME, 15.0, t(d, 9, 6))) is None    # six minutes: passing through
    assert s.timeline(d, d) == []


@pytest.mark.parametrize("body", [
    [], {"lat": 0, "lon": 0, "accuracy_m": 5, "arrived": None, "departed": None},
    {"lat": 91, "lon": 0, "accuracy_m": 5, "arrived": "2026-10-18T09:00:00Z", "departed": None},
    {"lat": 0, "lon": 0, "accuracy_m": 5, "arrived": "2026-10-18T09:00:00", "departed": None},
    {"lat": 0, "lon": 0, "accuracy_m": 5, "arrived": "2026-10-18T10:00:00Z", "departed": "2026-10-18T09:00:00Z"},
    {"lat": 0, "lon": 0, "accuracy_m": 5, "arrived": "2099-01-01T00:00:00Z", "departed": None},
    {"lat": 0, "lon": 0, "accuracy_m": 5, "arrived": "2026-10-18T09:00:00Z", "departed": None, "poi": 5},
])
def test_a_body_that_is_not_a_visit_is_refused(body: Any) -> None:
    with pytest.raises(places.BadVisit):
        places.parse_visit(body)


def test_a_visit_body_parses_and_drops_an_odd_category() -> None:
    v = places.parse_visit({"lat": 1, "lon": 2, "accuracy_m": 30, "arrived": "2026-10-18T09:00:00Z",
                            "departed": None, "place": " 1st  Ave ", "poi": "Crux", "category": "rock climbing!"})
    assert (v.place, v.poi, v.category, v.departed) == ("1st Ave", "Crux", None, None)


# ---- home, work and habits ---------------------------------------------------------------------------------------

def test_home_and_work_are_inferred_and_named(tmp_path: Path) -> None:
    s = store(tmp_path)
    four_weeks(s)
    labels = s.infer_labels()
    assert labels["home"] is not None and labels["work"] is not None and labels["home"] != labels["work"]
    names = [v["place"] for v in s.timeline(NOW.date(), NOW.date())]
    assert names == ["Home", "Work"]


def test_habits_are_the_usual_days_times_and_stays(tmp_path: Path) -> None:
    s = store(tmp_path)
    four_weeks(s)
    out = s.patterns()
    by = {p["place"]: p for p in out["places"]}
    assert by["Equinox"]["usual_days"] == "Mon, Wed and Fri"
    assert by["Equinox"]["usual_arrival"] == "18:00" and by["Equinox"]["usual_stay"] == "1 h"
    assert by["Work"]["usual_days"] == "weekdays" and by["Work"]["usual_arrival"] == "09:00"
    assert "usual_days" not in by["Green City Market"] if "Green City Market" in by else True
    assert any("leave home around 08:30" in line for line in out["routine"])
    assert any("home by" in line for line in out["routine"])


def test_a_day_digest_says_the_day_in_order_and_what_was_missed(tmp_path: Path) -> None:
    wednesday = NOW.date() - timedelta(days=1)
    s = store(tmp_path)
    four_weeks(s, skip_gym_on=wednesday)
    out = s.digest(wednesday)
    assert [x["place"] for x in out["stops"]] == ["Home", "Work", "Home"]
    assert out["stops"][1]["from"] == "09:00" and out["stops"][1]["to"] == "17:30"
    assert out["at_work"] == "8 h 30 min" and "at_home" in out
    assert out["missed"] == ["Equinox (usually Mon, Wed and Fri)"]
    assert out["text"].startswith("Wednesday 2026-10-21: Home 00:00-08:30")
    monday = wednesday - timedelta(days=2)
    assert "missed" not in s.digest(monday) and "Equinox" in [x["place"] for x in s.digest(monday)["stops"]]


def test_today_s_digest_ends_now_and_a_week_digest_ranks_places(tmp_path: Path) -> None:
    s = store(tmp_path)
    four_weeks(s)
    today = s.digest(NOW.date())
    assert today["stops"][-1] == {"place": "Work", "from": "09:00", "to": "now", "stayed": "3 h"}
    assert "missed" not in today
    week = s.week(NOW.date() - timedelta(days=7))
    assert week["places"][0]["place"] == "Home" and week["places"][1]["place"] == "Work"
    assert week["places"][1]["days"] == 5 and week["text"].startswith("Week of 2026-10-12: Home")
    assert week["places"][0]["days"] == 7                   # a night at home touches two days; all 7 are counted


def test_new_places_show_in_their_day(tmp_path: Path) -> None:
    s = store(tmp_path)
    d = NOW.date()
    visit(s, MARKET, t(d, 9), t(d, 10), poi="Green City Market")
    assert s.digest(d)["new_places"] == ["Green City Market"]
    assert "new_places" not in s.digest(d - timedelta(days=1))


# ---- naming, things, here ----------------------------------------------------------------------------------------

def test_the_owner_names_where_they_are_and_renames_it(tmp_path: Path) -> None:
    e = env(tmp_path)
    s = places.Places(environ=e)
    assert s.name_place("Mom's")["reason"] == places.NO_HERE
    share_fix(e, MARKET)
    assert s.name_place("Mom's") == {"ok": True, "id": 1, "place": "Mom's", "address": "a street"}
    assert s.name_place("Mom and Dad's", "mom's")["place"] == "Mom and Dad's"
    assert s.name_place("x", "nowhere I know")["ok"] is False


def test_things_are_kept_where_the_owner_is_and_found_with_how_far(tmp_path: Path) -> None:
    e = env(tmp_path)
    s = places.Places(environ=e)
    share_fix(e, MARKET)
    assert s.remember_thing("my car", "level 3, row F")["ok"]
    found = s.find_thing("the car")
    assert found["ok"] and found["thing"] == "my car" and found["note"] == "level 3, row F"
    assert found["from_here"] == "right here" and found["map"].startswith("https://maps.apple.com/?ll=")
    share_fix(e, HOME)
    assert s.find_thing("car")["from_here"] == "about 1.9 km"           # 1.67 km north, 0.83 km east
    assert [x["thing"] for x in s.list_things()["things"]] == ["my car"]
    miss = s.find_thing("bike")
    assert miss["ok"] is False and miss["things"] == ["my car"]
    assert s.forget_thing("car")["ok"] and s.list_things()["things"] == []


def test_a_thing_at_a_known_place_needs_no_fix(tmp_path: Path) -> None:
    s = store(tmp_path)
    visit(s, MARKET, t(NOW.date(), 9), t(NOW.date(), 10), poi="Sam's")
    out = s.remember_thing("jacket", "", "Sam's")
    assert out == {"ok": True, "thing": "jacket", "place": "Sam's"}
    assert s.find_thing("jacket")["place"] == "Sam's"


def test_where_the_owner_is_now_is_said_by_name_in_the_system_context(tmp_path: Path,
                                                                      monkeypatch: pytest.MonkeyPatch) -> None:
    e = env(tmp_path)
    for k, v in e.items():
        monkeypatch.setenv(k, v)
    s = places.Places(environ=e)
    assert places.here_line() == ""                         # no fix, nothing said
    arrived = datetime.now(timezone.utc) - timedelta(minutes=40)
    visit(s, GYM, arrived, None, poi="Equinox", category="fitnessCenter")
    share_fix(e, GYM)
    since = arrived.astimezone(PT).strftime("%H:%M")
    line = f"Where the owner is now, from their phone: Equinox (fitness center), since {since}."
    assert places.here_line() == line
    assert line in system_context.context(now=datetime(2026, 10, 22, 12, 0, tzinfo=PT))
    reading = phone_location.reading()
    assert reading["at"] == {"place": "Equinox", "kind": "fitness center", "since": since}
    monkeypatch.setenv("EVE_PLACES", "0")
    assert places.here_line() == "" and line not in system_context.context()
    assert "at" not in phone_location.reading()


# ---- tools ---------------------------------------------------------------------------------------------------------

def test_the_tools_ride_with_phone_location_once_the_phone_has_shared(tmp_path: Path) -> None:
    assert phone_location.PLACES_TOOL_NAMES == places.TOOL_NAMES
    assert phone_location.tools() == []
    share_fix(dict(os.environ), MARKET)
    names = [x["name"] for x in phone_location.tools()]
    assert names == [phone_location.TOOL_NAME, *places.TOOL_NAMES]
    for tool in places.TOOLS:
        params = tool["parameters"]
        assert tool["strict"] and set(params["required"]) == set(params["properties"])
    os.environ["EVE_PLACES"] = "0"
    try:
        assert [x["name"] for x in phone_location.tools()] == [phone_location.TOOL_NAME]
        out = asyncio.run(phone_location.handle(places.RECALL, {"what": "day", "start_date": "", "end_date": ""}))
        assert out == {"ok": False, "reason": places.OFF_REASON}
    finally:
        del os.environ["EVE_PLACES"]


def test_the_recall_tool_answers_each_kind_and_refuses_bad_dates(tmp_path: Path) -> None:
    s = store(tmp_path)
    four_weeks(s)
    e = env(tmp_path)

    def recall(what: str, start: str = "", end: str = "") -> dict[str, Any]:
        return places.call(places.RECALL, {"what": what, "start_date": start, "end_date": end}, s, e)

    assert recall("day")["day"] == "Thursday 2026-10-22"
    assert recall("week", "2026-10-14")["week"] == "2026-10-12 to 2026-10-18"
    assert len(recall("timeline", "2026-10-19", "2026-10-21")["visits"]) == 9
    assert recall("habits")["places"]
    assert recall("places")["places"][0]["place"] == "Home"
    assert recall("timeline", "2026-10-21", "2026-10-19")["ok"] is False
    assert recall("day", "21 Oct")["reason"] == "dates are YYYY-MM-DD"
    thing = places.call(places.THING, {"action": "list", "thing": "", "note": "", "place": ""}, s, e)
    assert thing == {"ok": True, "things": []}


# ---- keeping and forgetting ----------------------------------------------------------------------------------------

def test_the_file_is_owner_only_and_old_evidence_is_pruned(tmp_path: Path) -> None:
    s = store(tmp_path)
    old = NOW - timedelta(days=places.FIX_DAYS + 1)
    s.record_fix(phone_location.Fix(*HOME, 10.0, old.astimezone(timezone.utc)))
    s.record_fix(phone_location.Fix(*HOME, 10.0, (NOW - timedelta(hours=1)).astimezone(timezone.utc)))
    assert (s.path.stat().st_mode & 0o777) == 0o600
    assert s.prune()["fixes"] == 1


def test_forget_wipes_everything_with_the_latest_fix(tmp_path: Path) -> None:
    e = env(tmp_path)
    s = places.Places(environ=e)
    share_fix(e, MARKET)
    visit(s, MARKET, datetime.now(timezone.utc) - timedelta(hours=2), datetime.now(timezone.utc))
    s.remember_thing("bike")
    forgot = s.forget()
    assert forgot["places"] >= 1 and forgot["visits"] == 1 and forgot["things"] == 1
    assert s.summary() == {"places": 0, "visits": 0, "things": 0, "since": None}
    assert not Path(e["EVE_PHONE_LOCATION_FILE"]).exists()
