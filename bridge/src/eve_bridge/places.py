"""Places: where the owner goes, how long they stay, what they do by habit, and where they left things.

Owner, 2026-10-18: "make a location service that tracks location ... digests it tracks patterns habits where things
are ... cuz rn u get pure coordinates". Until then eve kept only the phone's latest fix (phone_location.py) and the
brain saw a latitude and a longitude. This module keeps the history on this Mac and turns it into words: "Home since
18:40", "Equinox, usually Mon, Wed and Fri around 07:10, about 1 h", "yesterday: Home, Work 9:10-17:45, Equinox".

**What comes in.** Two kinds of evidence, both from the Eve Link app over the link (link_chat.py):

- every fix the phone sends to ``POST /api/link/location`` (``record_fix``): foreground updates, and in the background
  iOS's significant-change moves (about 500 m);
- iOS's visits (``POST /api/link/visit``, ``record_visit``): CLVisit's arrival and departure at a place, which iOS
  works out itself at almost no battery cost, with Always permission. The phone adds the nearest point of interest
  (Apple's MapKit, within 60 m) so a place has a name like "Equinox" and a kind like "fitness center".

A visit from iOS is the better evidence. Without one (While Using permission, or iOS not reporting) a stay is read
from the fixes: fixes within STAY_RADIUS_M of each other for at least MIN_STAY_S are a visit, and when a fix lands far
away the stay ends at that fix minus the travel time at TRAVEL_MPS (a significant-change fix means the phone did not
move far before it). An iOS visit that covers the same time replaces the stays read from fixes.

**Places.** A visit joins the nearest known place within PLACE_RADIUS_M (wider for a vague fix, up to
MAX_MATCH_RADIUS_M) or the same point of interest within POI_MATCH_M, else it starts a new one. Home and work are
inferred (``infer_labels``): home is where the nights are spent, work is where weekday days are spent, each over the
last LABEL_DAYS days. The owner's own name for a place ("Mom's", ``name_place``) wins over both.

**Habits.** ``patterns``: for each place over the last PATTERN_WEEKS weeks, the weekdays it is usually visited on,
the usual arrival time and the usual stay; and the home routine (when the owner leaves home and gets back on
weekdays). Plain medians and counts, no model: a habit is something that happened, not a guess.

**Digests.** ``digest(day)``: the day's places in order with times, time at home, at work and out, about how far
the phone moved, the places that were new, and the usual places that were missed. ``week(day)``: the week's top
places, new places and the routines. Both are computed when asked, so a visit the phone sends late is never left out.

**Where things are.** ``remember_thing``: "my car is here", "I left my jacket at Sam's" keeps the thing at where the
owner is now (or a named place); ``find_thing`` says where and how far from here.

**Privacy.** Everything is one SQLite file on this Mac (``DEFAULT_FILE``, ``EVE_PLACES_FILE`` moves it), owner-only
(0600). Nothing here sends anything anywhere; the brain's tools answer with place names and times. Raw fixes are kept
FIX_DAYS days, visits and places ``EVE_PLACES_DAYS`` (KEEP_DAYS by default), things until forgotten. ``forget``
wipes all of it, with the latest fix (the app's Settings, ``eve-bridge places forget``). Logs carry counts, never a
place or a coordinate. ``EVE_PLACES=0`` turns all of it off: nothing is kept and no tool is offered.
"""

from __future__ import annotations

import contextlib
import json
import logging
import math
import os
import re
import sqlite3
import statistics
import threading
from dataclasses import dataclass
from datetime import date, datetime, time, timedelta, timezone, tzinfo
from pathlib import Path
from typing import Any, Callable, Iterable, Mapping, Optional
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

from . import phone_location

log = logging.getLogger(__name__)

SWITCH = "EVE_PLACES"
FILE_ENV = "EVE_PLACES_FILE"
DAYS_ENV = "EVE_PLACES_DAYS"
DEFAULT_FILE = "~/.config/eve-bridge/places.db"
KEEP_DAYS = 365                    # visits and places
FIX_DAYS = 30                      # raw fixes: the stays are read from them as they come
STAY_RADIUS_M = 150.0              # fixes this close together are one stay
MIN_STAY_S = 10 * 60               # shorter is passing through
TRAVEL_MPS = 13.9                  # 50 km/h: a stay read from fixes ends this long before the far fix
PLACE_RADIUS_M = 120.0             # a visit this close to a place is at that place
MAX_MATCH_RADIUS_M = 250.0         # a vague fix widens the match this far, no further
POI_MATCH_M = 300.0                # the same point of interest this close is the same place
VAGUE_FIX_M = 1000.0               # a fix vaguer than this says nothing about a stay
VISIT_SAME_S = 120.0               # iOS's departure carries the arrival it ends; this close is the same visit
LABEL_DAYS = 21
LABEL_MIN_DAYS = 3                 # nights (home) or weekdays (work) with at least LABEL_MIN_HOURS there
LABEL_MIN_HOURS = 2.0
WORK_MIN_HOURS = 10.0              # in LABEL_DAYS
PATTERN_WEEKS = 8
MAX_TIMELINE = 60
MAX_THINGS = 200
MAX_NAME_CHARS = 60
MAX_NOTE_CHARS = 200
LABEL_TITLES = {"home": "Home", "work": "Work"}
WEEKDAYS = ("Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun")

OFF_REASON = "Places are off on this Mac (EVE_PLACES=0)."
NO_HERE = "the phone has not shared where it is in the last hour"

_CONTROL = re.compile(r"[\x00-\x1f\x7f-\x9f  ]")
_DAY = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_lock = threading.Lock()           # one connection at a time in this process: sqlite3 objects are not shared


# ---- switches and paths ------------------------------------------------------------------------------------------

def enabled(environ: Optional[Mapping[str, str]] = None) -> bool:
    """On unless EVE_PLACES is 0/false/no/off, and only while the phone's location is on."""
    env = os.environ if environ is None else environ
    raw = (env.get(SWITCH) or "").strip().lower()
    return raw not in ("0", "false", "no", "off") and phone_location.enabled(env)


def state_path(environ: Optional[Mapping[str, str]] = None) -> Path:
    env = os.environ if environ is None else environ
    raw = (env.get(FILE_ENV) or os.environ.get(FILE_ENV) or DEFAULT_FILE).strip()
    return Path(raw).expanduser()


def keep_days(environ: Optional[Mapping[str, str]] = None) -> int:
    env = os.environ if environ is None else environ
    try:
        days = int((env.get(DAYS_ENV) or "").strip() or KEEP_DAYS)
    except ValueError:
        return KEEP_DAYS
    return max(FIX_DAYS, min(days, 3650))


def zone(environ: Optional[Mapping[str, str]] = None) -> tzinfo:
    """The owner's zone: EVE_TIMEZONE, else this Mac's."""
    env = os.environ if environ is None else environ
    name = (env.get("EVE_TIMEZONE") or "").strip()
    if name:
        try:
            return ZoneInfo(name)
        except (ZoneInfoNotFoundError, ValueError):
            pass
    return datetime.now().astimezone().tzinfo or timezone.utc


# ---- geometry and words ------------------------------------------------------------------------------------------

def distance_m(lat1: float, lon1: float, lat2: float, lon2: float) -> float:
    """Great-circle distance in meters (haversine)."""
    r = 6_371_008.8
    p1, p2 = math.radians(lat1), math.radians(lat2)
    dp, dl = math.radians(lat2 - lat1), math.radians(lon2 - lon1)
    h = math.sin(dp / 2) ** 2 + math.cos(p1) * math.cos(p2) * math.sin(dl / 2) ** 2
    return 2 * r * math.asin(min(1.0, math.sqrt(h)))


def clean(raw: Any, limit: int) -> Optional[str]:
    if raw is None:
        return None
    if not isinstance(raw, str):
        raise ValueError("must be a string or null")
    text = " ".join(_CONTROL.sub(" ", raw).split())
    if len(text) > limit:
        raise ValueError(f"is at most {limit} characters")
    return text or None


def duration(secs: float) -> str:
    """'45 min', '2 h 10 min', '3 h'."""
    minutes = max(1, int(round(secs / 60)))
    if minutes < 60:
        return f"{minutes} min"
    h, m = divmod(minutes, 60)
    return f"{h} h" if m == 0 else f"{h} h {m} min"


def clock(minute_of_day: float) -> str:
    m = int(round(minute_of_day)) % (24 * 60)
    return f"{m // 60:02d}:{m % 60:02d}"


def distance_words(m: float) -> str:
    if m < 1000:
        return f"about {max(10, int(round(m / 10.0)) * 10)} m"
    return f"about {m / 1000:.1f} km" if m < 10_000 else f"about {round(m / 1000)} km"


def days_words(days: Iterable[int]) -> str:
    """'weekdays', 'weekends', 'every day', 'Mon, Wed and Fri'."""
    ds = sorted(set(days))
    if ds == list(range(7)):
        return "every day"
    if ds == [0, 1, 2, 3, 4]:
        return "weekdays"
    if ds == [5, 6]:
        return "weekends"
    names = [WEEKDAYS[d] for d in ds]
    return names[0] if len(names) == 1 else ", ".join(names[:-1]) + " and " + names[-1]


def category_words(raw: Optional[str]) -> Optional[str]:
    """MapKit's 'fitnessCenter' as 'fitness center'."""
    if not raw:
        return None
    return re.sub(r"(?<=[a-z])(?=[A-Z])", " ", raw).lower()


# ---- the evidence ------------------------------------------------------------------------------------------------

@dataclass(frozen=True)
class Visit:
    """iOS's CLVisit, as the app posts it: an arrival (departed None), a departure, or both."""
    lat: float
    lon: float
    accuracy_m: float
    arrived: Optional[datetime]        # aware, UTC
    departed: Optional[datetime]
    place: Optional[str] = None        # the reverse-geocoded line
    poi: Optional[str] = None          # the nearest point of interest's name
    category: Optional[str] = None     # its MapKit category, e.g. fitnessCenter


class BadVisit(ValueError):
    """A body that is not a visit; its message is the route's 400 reason (no value from the body in it)."""


def _when(raw: Any, key: str) -> Optional[datetime]:
    if raw is None:
        return None
    try:
        return phone_location._when(raw)
    except phone_location.BadFix:
        raise BadVisit(f"{key} must be an ISO 8601 time with a zone, or null") from None


def parse_visit(body: Any, now: Optional[datetime] = None) -> Visit:
    """The app's visit body, or BadVisit with the reason."""
    if not isinstance(body, dict):
        raise BadVisit("the body must be a JSON object")
    try:
        lat = phone_location._number(body, "lat", -90.0, 90.0)
        lon = phone_location._number(body, "lon", -180.0, 180.0)
        acc = phone_location._number(body, "accuracy_m", 0.0, phone_location.MAX_ACCURACY_M)
    except phone_location.BadFix as e:
        raise BadVisit(str(e)) from None
    arrived, departed = _when(body.get("arrived"), "arrived"), _when(body.get("departed"), "departed")
    if arrived is None and departed is None:
        raise BadVisit("a visit needs arrived or departed")
    now = now if now is not None else datetime.now(timezone.utc)
    for t in (arrived, departed):
        if t is not None and (t - now).total_seconds() > phone_location.FUTURE_SECS:
            raise BadVisit("a visit's time is in the future")
    if arrived is not None and departed is not None and departed < arrived:
        raise BadVisit("departed is before arrived")
    try:
        place = clean(body.get("place"), phone_location.MAX_PLACE_CHARS)
        poi = clean(body.get("poi"), phone_location.MAX_PLACE_CHARS)
        category = clean(body.get("category"), 60)
    except ValueError as e:
        raise BadVisit(f"place, poi and category: {e}") from None
    if category is not None and not re.fullmatch(r"[A-Za-z]+", category):
        category = None
    assert lat is not None and lon is not None and acc is not None
    return Visit(lat, lon, acc, arrived, departed, place, poi, category)


# ---- the store ---------------------------------------------------------------------------------------------------

SCHEMA = """
create table if not exists fixes (at real primary key, lat real, lon real, acc real, speed real, place text);
create table if not exists places (
    id integer primary key, lat real not null, lon real not null, n integer not null default 1,
    name text, label text, poi text, category text, address text, first_seen real, last_seen real);
create table if not exists visits (
    id integer primary key, place_id integer not null, arrived real not null, departed real,
    lat real not null, lon real not null, acc real not null, source text not null);
create index if not exists visits_arrived on visits (arrived);
create table if not exists things (
    id integer primary key, thing text not null, key text not null unique, note text,
    lat real, lon real, acc real, place_id integer, at real not null);
create table if not exists meta (key text primary key, value text);
"""


class Places:
    """The history in one SQLite file. Every method opens, works and closes under the module lock: calls come from
    worker threads (asyncio.to_thread), a few a minute at most. ``now`` is lent for tests."""

    def __init__(self, path: Optional[Path] = None, *, environ: Optional[Mapping[str, str]] = None,
                 now: Callable[[], datetime] = lambda: datetime.now(timezone.utc)) -> None:
        self.env = os.environ if environ is None else environ
        self.path = path if path is not None else state_path(self.env)
        self.zone = zone(self.env)
        self._now = now

    # ---- plumbing --------------------------------------------------------------------------------------------

    @contextlib.contextmanager
    def _db(self) -> Any:
        with _lock:
            new = not self.path.exists()
            if new:
                self.path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
            con = sqlite3.connect(str(self.path), timeout=5)
            try:
                if new:
                    os.chmod(self.path, 0o600)
                con.row_factory = sqlite3.Row
                con.executescript(SCHEMA)
                yield con
                con.commit()
            except BaseException:
                con.rollback()
                raise
            finally:
                con.close()

    def _ts(self) -> float:
        return self._now().timestamp()

    def _local(self, ts: float) -> datetime:
        return datetime.fromtimestamp(ts, self.zone)

    def _day_bounds(self, day: date) -> tuple[float, float]:
        start = datetime.combine(day, time.min, tzinfo=self.zone)
        end = datetime.combine(day + timedelta(days=1), time.min, tzinfo=self.zone)
        return start.timestamp(), end.timestamp()

    @staticmethod
    def _meta(con: sqlite3.Connection, key: str) -> Optional[Any]:
        row = con.execute("select value from meta where key = ?", (key,)).fetchone()
        return json.loads(row["value"]) if row else None

    @staticmethod
    def _set_meta(con: sqlite3.Connection, key: str, value: Any) -> None:
        if value is None:
            con.execute("delete from meta where key = ?", (key,))
        else:
            con.execute("insert or replace into meta (key, value) values (?, ?)", (key, json.dumps(value)))

    # ---- places ----------------------------------------------------------------------------------------------

    def _match(self, con: sqlite3.Connection, lat: float, lon: float, acc: float,
               poi: Optional[str]) -> Optional[sqlite3.Row]:
        reach = max(PLACE_RADIUS_M, min(acc, MAX_MATCH_RADIUS_M))
        best: Optional[tuple[float, sqlite3.Row]] = None
        for row in con.execute("select * from places"):
            d = distance_m(lat, lon, row["lat"], row["lon"])
            if poi and row["poi"] and row["poi"].lower() == poi.lower() and d <= POI_MATCH_M:
                return row
            if d <= reach and (best is None or d < best[0]):
                best = (d, row)
        return best[1] if best else None

    def _place_for(self, con: sqlite3.Connection, lat: float, lon: float, acc: float, at: float, *,
                   place: Optional[str] = None, poi: Optional[str] = None, category: Optional[str] = None) -> int:
        row = self._match(con, lat, lon, acc, poi)
        if row is None:
            cur = con.execute("insert into places (lat, lon, n, poi, category, address, first_seen, last_seen) "
                              "values (?, ?, 1, ?, ?, ?, ?, ?)", (lat, lon, poi, category, place, at, at))
            return int(cur.lastrowid or 0)
        n = min(int(row["n"]), 20)        # the center follows the visits, but a long history does not freeze it
        con.execute("update places set lat = ?, lon = ?, n = n + 1, poi = coalesce(poi, ?), "
                    "category = coalesce(category, ?), address = coalesce(?, address), "
                    "first_seen = min(coalesce(first_seen, ?), ?), last_seen = max(coalesce(last_seen, ?), ?) "
                    "where id = ?",
                    ((row["lat"] * n + lat) / (n + 1), (row["lon"] * n + lon) / (n + 1), poi, category, place,
                     at, at, at, at, row["id"]))
        return int(row["id"])

    @staticmethod
    def title(row: Mapping[str, Any]) -> str:
        """A place in words: the owner's name, else Home / Work, else its point of interest, else its address."""
        if row["name"]:
            return str(row["name"])
        if row["label"] in LABEL_TITLES:
            return LABEL_TITLES[row["label"]]
        if row["poi"]:
            return str(row["poi"])
        if row["address"]:
            return str(row["address"])
        return "an unnamed place"

    def _describe(self, row: Mapping[str, Any]) -> dict[str, Any]:
        out: dict[str, Any] = {"id": int(row["id"]), "place": self.title(row)}
        kind = category_words(row["category"])
        if kind:
            out["kind"] = kind
        if row["label"]:
            out["is"] = row["label"]
        if row["address"] and row["address"] != out["place"]:
            out["address"] = row["address"]
        return out

    # ---- evidence in -----------------------------------------------------------------------------------------

    def record_fix(self, fix: phone_location.Fix) -> Optional[dict[str, Any]]:
        """Keep one fix and move the stay read from fixes along. Returns the visit it closed, if any."""
        at = fix.at.timestamp()
        with self._db() as con:
            con.execute("insert or ignore into fixes (at, lat, lon, acc, speed, place) values (?, ?, ?, ?, ?, ?)",
                        (at, fix.lat, fix.lon, fix.accuracy_m, fix.speed_mps, fix.place))
            if fix.accuracy_m > VAGUE_FIX_M:
                return None
            stay = self._meta(con, "stay")
            if stay is not None and at < stay["last"]:
                return None                                  # a late fix: kept, but the stay has moved on
            if stay is None:
                self._set_meta(con, "stay", self._new_stay(fix, at))
                return None
            d = distance_m(fix.lat, fix.lon, stay["lat"], stay["lon"])
            if d <= max(STAY_RADIUS_M, min(max(fix.accuracy_m, stay["acc"]), MAX_MATCH_RADIUS_M)):
                n = stay["n"]
                stay.update(lat=(stay["lat"] * n + fix.lat) / (n + 1), lon=(stay["lon"] * n + fix.lon) / (n + 1),
                            n=min(n + 1, 20), last=at, acc=min(stay["acc"], fix.accuracy_m),
                            place=fix.place or stay.get("place"))
                self._set_meta(con, "stay", stay)
                return None
            # moved: the stay ended before this fix, by the time it takes to get here
            end = min(at, max(stay["last"], at - d / TRAVEL_MPS))
            closed = None
            if end - stay["arrived"] >= MIN_STAY_S:
                closed = self._insert_visit(con, stay["lat"], stay["lon"], stay["acc"], stay["arrived"], end,
                                            "fixes", place=stay.get("place"))
            self._set_meta(con, "stay", self._new_stay(fix, at))
            return closed

    @staticmethod
    def _new_stay(fix: phone_location.Fix, at: float) -> dict[str, Any]:
        return {"lat": fix.lat, "lon": fix.lon, "acc": fix.accuracy_m, "arrived": at, "last": at, "n": 1,
                "place": fix.place}

    def _insert_visit(self, con: sqlite3.Connection, lat: float, lon: float, acc: float, arrived: float,
                      departed: Optional[float], source: str, *, place: Optional[str] = None,
                      poi: Optional[str] = None, category: Optional[str] = None) -> Optional[dict[str, Any]]:
        if source == "fixes":
            # iOS already reported this stretch here: its visit is the better evidence
            for v in con.execute("select * from visits where source = 'visit' and arrived < ? "
                                 "and coalesce(departed, ?) > ?", (departed or arrived, departed or arrived, arrived)):
                if distance_m(lat, lon, v["lat"], v["lon"]) <= POI_MATCH_M:
                    return None
        pid = self._place_for(con, lat, lon, acc, arrived, place=place, poi=poi, category=category)
        cur = con.execute("insert into visits (place_id, arrived, departed, lat, lon, acc, source) "
                          "values (?, ?, ?, ?, ?, ?, ?)", (pid, arrived, departed, lat, lon, acc, source))
        if source == "visit":
            self._drop_fix_stays(con, lat, lon, arrived, departed)
        return {"id": int(cur.lastrowid or 0), "place_id": pid}

    @staticmethod
    def _drop_fix_stays(con: sqlite3.Connection, lat: float, lon: float, arrived: float,
                        departed: Optional[float]) -> None:
        end = departed if departed is not None else float("inf")
        for v in con.execute("select id, lat, lon, arrived, departed from visits where source = 'fixes' "
                             "and arrived < ? and coalesce(departed, arrived) > ?", (end, arrived)).fetchall():
            if distance_m(lat, lon, v["lat"], v["lon"]) <= POI_MATCH_M:
                con.execute("delete from visits where id = ?", (v["id"],))

    def record_visit(self, visit: Visit) -> str:
        """Keep one iOS visit: an arrival opens a visit, a departure closes the one it ends (or adds it whole).
        Returns what was done, for the log."""
        arrived = visit.arrived.timestamp() if visit.arrived else None
        departed = visit.departed.timestamp() if visit.departed else None
        with self._db() as con:
            kw = {"place": visit.place, "poi": visit.poi, "category": visit.category}
            if arrived is not None:
                same = con.execute("select * from visits where source = 'visit' and abs(arrived - ?) <= ?",
                                   (arrived, VISIT_SAME_S)).fetchall()
                same = [v for v in same if distance_m(visit.lat, visit.lon, v["lat"], v["lon"]) <= POI_MATCH_M]
                if same:
                    if departed is not None:
                        con.execute("update visits set departed = ? where id = ?", (departed, same[0]["id"]))
                        con.execute("update places set last_seen = max(coalesce(last_seen, ?), ?), "
                                    "poi = coalesce(poi, ?), category = coalesce(category, ?) where id = ?",
                                    (departed, departed, visit.poi, visit.category, same[0]["place_id"]))
                        self._drop_fix_stays(con, visit.lat, visit.lon, arrived, departed)
                        return "departure"
                    return "repeat"
                # an arrival while another visit is still open: that one ended by now
                con.execute("update visits set departed = ? where source = 'visit' and departed is null "
                            "and arrived < ?", (arrived, arrived))
                self._insert_visit(con, visit.lat, visit.lon, visit.accuracy_m, arrived, departed, "visit", **kw)
                return "visit" if departed is not None else "arrival"
            # a departure whose arrival iOS did not see: close the open visit here, if there is one
            assert departed is not None
            open_ = con.execute("select * from visits where departed is null and arrived <= ? "
                                "order by arrived desc limit 1", (departed,)).fetchone()
            if open_ is not None and distance_m(visit.lat, visit.lon, open_["lat"], open_["lon"]) <= POI_MATCH_M:
                con.execute("update visits set departed = ? where id = ?", (departed, open_["id"]))
                return "departure"
            return "ignored"

    # ---- labels ----------------------------------------------------------------------------------------------

    def _visits(self, con: sqlite3.Connection, start: float, end: float) -> list[sqlite3.Row]:
        return con.execute("select * from visits where arrived < ? and coalesce(departed, ?) > ? order by arrived",
                           (end, self._ts(), start)).fetchall()

    def _overlap_hours(self, visits: list[sqlite3.Row], window: Callable[[date], Optional[tuple[time, time]]],
                       start_day: date, end_day: date, now: float) -> dict[int, tuple[float, set[date]]]:
        """Per place: hours inside ``window`` on each day, and the days with at least LABEL_MIN_HOURS."""
        out: dict[int, tuple[float, set[date]]] = {}
        per_day: dict[tuple[int, date], float] = {}
        day = start_day
        while day <= end_day:
            span = window(day)
            if span is not None:
                w0 = datetime.combine(day, span[0], tzinfo=self.zone).timestamp()
                w1 = datetime.combine(day, span[1], tzinfo=self.zone).timestamp()
                for v in visits:
                    a, b = v["arrived"], v["departed"] if v["departed"] is not None else now
                    h = max(0.0, min(b, w1) - max(a, w0)) / 3600
                    if h > 0:
                        per_day[(v["place_id"], day)] = per_day.get((v["place_id"], day), 0.0) + h
            day += timedelta(days=1)
        for (pid, d), h in per_day.items():
            total, days = out.get(pid, (0.0, set()))
            if h >= LABEL_MIN_HOURS:
                days = days | {d}
            out[pid] = (total + h, days)
        return out

    def infer_labels(self) -> dict[str, Optional[int]]:
        """Home: the most night hours (00:00-06:00) over LABEL_DAYS, on at least LABEL_MIN_DAYS nights. Work: the most
        weekday hours (09:00-17:00) at another place, on at least LABEL_MIN_DAYS days and WORK_MIN_HOURS in all."""
        now = self._ts()
        today = self._local(now).date()
        first = today - timedelta(days=LABEL_DAYS)
        start, _ = self._day_bounds(first)
        with self._db() as con:
            visits = self._visits(con, start, now)
            nights = self._overlap_hours(visits, lambda d: (time(0), time(6)), first, today, now)
            days = self._overlap_hours(visits, lambda d: (time(9), time(17)) if d.weekday() < 5 else None,
                                       first, today, now)
            home = max((p for p, (h, ds) in nights.items() if len(ds) >= LABEL_MIN_DAYS),
                       key=lambda p: nights[p][0], default=None)
            work = max((p for p, (h, ds) in days.items()
                        if p != home and len(ds) >= LABEL_MIN_DAYS and h >= WORK_MIN_HOURS),
                       key=lambda p: days[p][0], default=None)
            con.execute("update places set label = null where label in ('home', 'work')")
            if home is not None:
                con.execute("update places set label = 'home' where id = ?", (home,))
            if work is not None:
                con.execute("update places set label = 'work' where id = ?", (work,))
        return {"home": home, "work": work}

    # ---- now -------------------------------------------------------------------------------------------------

    def here(self) -> Optional[dict[str, Any]]:
        """Where the owner is now in words: the place and since when; None without a fix in the last hour.
        ``{"place", "since"?, "kind"?, "is"?}``; an unknown spot is the phone's own name for it."""
        fix = phone_location.fresh(self.env)
        if fix is None:
            return None
        with self._db() as con:
            open_ = con.execute("select v.arrived, p.* from visits v join places p on p.id = v.place_id "
                                "where v.departed is null and v.source = 'visit' order by v.arrived desc "
                                "limit 1").fetchone()
            if open_ is not None and distance_m(fix.lat, fix.lon, open_["lat"], open_["lon"]) <= POI_MATCH_M:
                out = self._describe(open_)
                out["since"] = self._local(open_["arrived"]).strftime("%H:%M")
                return out
            row = self._match(con, fix.lat, fix.lon, fix.accuracy_m, None)
            stay = self._meta(con, "stay")
        out = self._describe(row) if row is not None else {"place": fix.place or "somewhere new"}
        if row is None:
            out.pop("id", None)
        if stay is not None and distance_m(fix.lat, fix.lon, stay["lat"], stay["lon"]) <= STAY_RADIUS_M \
                and stay["last"] - stay["arrived"] >= MIN_STAY_S:
            out["since"] = self._local(stay["arrived"]).strftime("%H:%M")
        return out

    # ---- the timeline ----------------------------------------------------------------------------------------

    def timeline(self, first: date, last: date) -> list[dict[str, Any]]:
        start, _ = self._day_bounds(first)
        _, end = self._day_bounds(last)
        now = self._ts()
        with self._db() as con:
            rows = con.execute("select v.arrived, v.departed, v.source, p.* from visits v join places p "
                               "on p.id = v.place_id where v.arrived < ? and coalesce(v.departed, ?) > ? "
                               "order by v.arrived", (end, now, start)).fetchall()
        out = []
        for r in rows[-MAX_TIMELINE:]:
            a = self._local(r["arrived"])
            item = self._describe(r)
            item["arrived"] = a.strftime("%a %Y-%m-%d %H:%M")
            if r["departed"] is not None:
                item["left"] = self._local(r["departed"]).strftime("%a %Y-%m-%d %H:%M")
                item["stayed"] = duration(r["departed"] - r["arrived"])
            else:
                item["left"] = "still there"
                item["stayed"] = duration(now - r["arrived"])
            if r["source"] == "fixes":
                item["approximate"] = True
            out.append(item)
        return out

    # ---- habits ----------------------------------------------------------------------------------------------

    def _pattern_rows(self, con: sqlite3.Connection, now: float) -> list[sqlite3.Row]:
        since = now - PATTERN_WEEKS * 7 * 86400
        return con.execute("select v.place_id, v.arrived, v.departed from visits v where v.arrived >= ? "
                           "order by v.arrived", (since,)).fetchall()

    def _habit(self, place: sqlite3.Row, visits: list[sqlite3.Row], now: float) -> Optional[dict[str, Any]]:
        if len(visits) < 2:
            return None
        since = max(place["first_seen"] or now, now - PATTERN_WEEKS * 7 * 86400)
        weeks = max(1, min(PATTERN_WEEKS, math.ceil((now - since) / (7 * 86400))))
        dates_by_wd: dict[int, set[date]] = {}
        for v in visits:
            d = self._local(v["arrived"]).date()
            dates_by_wd.setdefault(d.weekday(), set()).add(d)
        need = max(2, math.ceil(weeks * 0.5))
        usual = sorted(wd for wd, ds in dates_by_wd.items() if len(ds) >= need)
        pick = [v for v in visits if self._local(v["arrived"]).weekday() in usual] if usual else visits
        arrivals = [self._local(v["arrived"]).hour * 60 + self._local(v["arrived"]).minute for v in pick]
        stays = [v["departed"] - v["arrived"] for v in pick if v["departed"] is not None]
        out: dict[str, Any] = {"visits": len(visits), "weeks": weeks,
                               "per_week": round(len(visits) / weeks, 1)}
        if usual:
            out["usual_days"] = days_words(usual)
            out["usual_arrival"] = clock(statistics.median(arrivals))
        if stays:
            out["usual_stay"] = duration(statistics.median(stays))
        return out

    def _routine(self, con: sqlite3.Connection, home: Optional[int], now: float) -> list[str]:
        """The home routine on weekdays: when the owner leaves home in the morning and is back in the evening."""
        if home is None:
            return []
        since = now - PATTERN_WEEKS * 7 * 86400
        rows = con.execute("select arrived, departed from visits where place_id = ? and arrived >= ? - 86400",
                           (home, since)).fetchall()
        leaves, backs = [], []
        for r in rows:
            if r["departed"] is not None:
                d = self._local(r["departed"])
                if d.weekday() < 5 and 5 <= d.hour < 12:
                    leaves.append(d.hour * 60 + d.minute)
            a = self._local(r["arrived"])
            if a.weekday() < 5 and 15 <= a.hour < 24 and r["arrived"] >= since:
                backs.append(a.hour * 60 + a.minute)
        out = []
        if len(leaves) >= 3:
            out.append(f"On weekdays you usually leave home around {clock(statistics.median(leaves))}.")
        if len(backs) >= 3:
            out.append(f"On weekdays you are usually home by {clock(statistics.median(backs))}.")
        return out

    def patterns(self) -> dict[str, Any]:
        """Each place's habit over the last PATTERN_WEEKS weeks, most visited first, and the home routine."""
        self._maybe_infer()
        now = self._ts()
        with self._db() as con:
            rows = self._pattern_rows(con, now)
            places = {r["id"]: r for r in con.execute("select * from places")}
            home = next((p for p, r in places.items() if r["label"] == "home"), None)
            routine = self._routine(con, home, now)
        by_place: dict[int, list[sqlite3.Row]] = {}
        for r in rows:
            by_place.setdefault(r["place_id"], []).append(r)
        habits = []
        for pid, vs in sorted(by_place.items(), key=lambda kv: -len(kv[1])):
            place = places.get(pid)
            if place is None:
                continue
            habit = self._habit(place, vs, now)
            if habit is None:
                continue
            habits.append({**self._describe(place), **habit})
        return {"weeks": PATTERN_WEEKS, "places": habits[:20], "routine": routine}

    def _maybe_infer(self) -> None:
        with self._db() as con:
            last = self._meta(con, "labelled") or 0
        if self._ts() - float(last) >= 3600:
            self.infer_labels()
            with self._db() as con:
                self._set_meta(con, "labelled", self._ts())

    # ---- digests ---------------------------------------------------------------------------------------------

    def digest(self, day: date) -> dict[str, Any]:
        """One day in words: the places in order, time at home / work / out, how far, what was new, what was missed."""
        self._maybe_infer()
        start, end = self._day_bounds(day)
        now = self._ts()
        today = self._local(now).date()
        with self._db() as con:
            rows = con.execute("select v.arrived, v.departed, v.place_id, v.source, p.* from visits v join places p "
                               "on p.id = v.place_id where v.arrived < ? and coalesce(v.departed, ?) > ? "
                               "order by v.arrived", (end, now, start)).fetchall()
            fixes = con.execute("select lat, lon, acc from fixes where at >= ? and at < ? order by at",
                                (start, end)).fetchall()
            new = con.execute("select * from places where first_seen >= ? and first_seen < ?",
                              (start, end)).fetchall()
            habit_rows = self._pattern_rows(con, min(now, start))
            places = {r["id"]: r for r in con.execute("select * from places")}
        stops, at = [], {"home": 0.0, "work": 0.0, "out": 0.0}
        seen: set[int] = set()
        for r in rows:
            a, b = max(r["arrived"], start), min(r["departed"] if r["departed"] is not None else now, end)
            if b <= a:
                continue
            seen.add(r["place_id"])
            at[r["label"] if r["label"] in at else "out"] += b - a
            stop = {"place": self.title(r), "from": self._local(a).strftime("%H:%M"),
                    "to": "now" if r["departed"] is None and day == today else self._local(b).strftime("%H:%M"),
                    "stayed": duration(b - a)}
            kind = category_words(r["category"])
            if kind:
                stop["kind"] = kind
            stops.append(stop)
        moved = 0.0
        prev = None
        for f in fixes:
            if f["acc"] > 200:
                continue
            if prev is not None:
                hop = distance_m(prev["lat"], prev["lon"], f["lat"], f["lon"])
                if hop > max(prev["acc"], f["acc"], 50):
                    moved += hop
            prev = f
        out: dict[str, Any] = {"day": day.strftime("%A %Y-%m-%d"), "stops": stops}
        for k in ("home", "work", "out"):
            if at[k] >= 60:
                out[f"at_{k}"] = duration(at[k])
        if moved >= 200:
            out["moved"] = distance_words(moved)
        if new:
            out["new_places"] = [self.title(p) for p in new]
        # usual places this weekday that were not visited (only once the day is over)
        if day < today:
            by_place: dict[int, list[sqlite3.Row]] = {}
            for r in habit_rows:
                by_place.setdefault(r["place_id"], []).append(r)
            missed = []
            for pid, vs in by_place.items():
                place = places.get(pid)
                if place is None or pid in seen or place["label"] == "home":
                    continue
                habit = self._habit(place, vs, start)
                if habit and "usual_days" in habit and day.weekday() in self._days_of(vs):
                    missed.append(f"{self.title(place)} (usually {habit['usual_days']})")
            if missed:
                out["missed"] = missed
        out["text"] = self._digest_text(out)
        return out

    def _days_of(self, visits: list[sqlite3.Row]) -> set[int]:
        weeks = max(1, min(PATTERN_WEEKS, math.ceil((visits[-1]["arrived"] - visits[0]["arrived"]) / (7 * 86400))))
        dates: dict[int, set[date]] = {}
        for v in visits:
            d = self._local(v["arrived"]).date()
            dates.setdefault(d.weekday(), set()).add(d)
        need = max(2, math.ceil(weeks * 0.5))
        return {wd for wd, ds in dates.items() if len(ds) >= need}

    @staticmethod
    def _digest_text(d: Mapping[str, Any]) -> str:
        if not d["stops"]:
            return f"{d['day']}: no places recorded."
        lines = [f"{d['day']}: " + "; ".join(f"{s['place']} {s['from']}-{s['to']} ({s['stayed']})"
                                              for s in d["stops"]) + "."]
        times = [f"{v} {k[3:]}" for k, v in d.items() if k.startswith("at_")]
        if times:
            lines.append("Time: " + ", ".join(times) + ".")
        if d.get("moved"):
            lines.append(f"Moved {d['moved']}.")
        if d.get("new_places"):
            lines.append("New: " + ", ".join(d["new_places"]) + ".")
        if d.get("missed"):
            lines.append("Missed: " + ", ".join(d["missed"]) + ".")
        return " ".join(lines)

    def week(self, day: date) -> dict[str, Any]:
        """The week (Monday to Sunday) holding ``day``: the top places by time, new places and the routine."""
        self._maybe_infer()
        monday = day - timedelta(days=day.weekday())
        start, _ = self._day_bounds(monday)
        _, end = self._day_bounds(monday + timedelta(days=6))
        now = self._ts()
        with self._db() as con:
            rows = con.execute("select v.arrived, v.departed, p.* from visits v join places p on p.id = v.place_id "
                               "where v.arrived < ? and coalesce(v.departed, ?) > ?", (end, now, start)).fetchall()
            new = con.execute("select * from places where first_seen >= ? and first_seen < ?", (start, end)).fetchall()
        total: dict[int, tuple[float, set[date], sqlite3.Row]] = {}
        for r in rows:
            a, b = max(r["arrived"], start), min(r["departed"] if r["departed"] is not None else now, end)
            if b <= a:
                continue
            t, ds, _ = total.get(r["id"], (0.0, set(), r))
            days, d = set(), self._local(a).date()
            while d <= self._local(b - 1).date():         # every day the stay touches, not only its first
                days.add(d)
                d += timedelta(days=1)
            total[r["id"]] = (t + b - a, ds | days, r)
        top = sorted(total.values(), key=lambda x: -x[0])[:8]
        out: dict[str, Any] = {
            "week": f"{monday.isoformat()} to {(monday + timedelta(days=6)).isoformat()}",
            "places": [{"place": self.title(r), "time": duration(t), "days": len(ds)} for t, ds, r in top],
            "places_visited": len(total)}
        if new:
            out["new_places"] = [self.title(p) for p in new]
        out["routine"] = self.patterns()["routine"]
        out["text"] = (f"Week of {monday.isoformat()}: " + "; ".join(
            f"{p['place']} {p['time']} on {p['days']} day{'s' if p['days'] != 1 else ''}" for p in out["places"])
            + "." + (f" New: {', '.join(out['new_places'])}." if new else "")) if top else \
            f"Week of {monday.isoformat()}: no places recorded."
        return out

    # ---- naming ----------------------------------------------------------------------------------------------

    def _find_place(self, con: sqlite3.Connection, which: str) -> Optional[sqlite3.Row]:
        """'' is where the owner is now; else a place by its name, label, point of interest or address."""
        if not which:
            fix = phone_location.fresh(self.env)
            if fix is None:
                return None
            row = self._match(con, fix.lat, fix.lon, fix.accuracy_m, None)
            if row is None:
                pid = self._place_for(con, fix.lat, fix.lon, fix.accuracy_m, fix.at.timestamp(), place=fix.place)
                row = con.execute("select * from places where id = ?", (pid,)).fetchone()
            return row
        key = which.strip().lower()
        rows = con.execute("select * from places").fetchall()
        for field in ("name", "label", "poi", "address"):
            for r in rows:
                if r[field] and str(r[field]).lower() == key:
                    return r
        for r in rows:
            if key in self.title(r).lower():
                return r
        return None

    def name_place(self, name: str, which: str = "") -> dict[str, Any]:
        """The owner's own name for a place ('' clears it): where they are now, or one named by ``which``."""
        try:
            clean_name = clean(name, MAX_NAME_CHARS)
        except ValueError as e:
            return {"ok": False, "reason": f"name {e}"}
        with self._db() as con:
            row = self._find_place(con, " ".join((which or "").split()))
            if row is None:
                return {"ok": False, "reason": NO_HERE if not which else "no known place goes by that name"}
            con.execute("update places set name = ? where id = ?", (clean_name, row["id"]))
            row = con.execute("select * from places where id = ?", (row["id"],)).fetchone()
        return {"ok": True, **self._describe(row)}

    # ---- things ----------------------------------------------------------------------------------------------

    @staticmethod
    def _key(thing: str) -> str:
        words = re.sub(r"[^\w\s]", " ", thing.lower()).split()
        return " ".join(w for w in words if w not in ("my", "the", "a", "an", "our"))

    def remember_thing(self, thing: str, note: str = "", which: str = "") -> dict[str, Any]:
        """Keep where a thing is: where the owner is now (the fix itself, for a car on a street), or a known place."""
        try:
            name = clean(thing, MAX_NAME_CHARS)
            note_text = clean(note, MAX_NOTE_CHARS)
        except ValueError as e:
            return {"ok": False, "reason": f"thing and note: {e}"}
        if not name or not self._key(name):
            return {"ok": False, "reason": "say which thing"}
        with self._db() as con:
            if con.execute("select count(*) from things").fetchone()[0] >= MAX_THINGS:
                return {"ok": False, "reason": f"eve keeps at most {MAX_THINGS} things; forget one first"}
            if which:
                row = self._find_place(con, " ".join(which.split()))
                if row is None:
                    return {"ok": False, "reason": "no known place goes by that name"}
                lat, lon, acc, pid = row["lat"], row["lon"], PLACE_RADIUS_M, row["id"]
            else:
                fix = phone_location.fresh(self.env)
                if fix is None:
                    return {"ok": False, "reason": NO_HERE}
                row = self._match(con, fix.lat, fix.lon, fix.accuracy_m, None)
                lat, lon, acc = fix.lat, fix.lon, fix.accuracy_m
                pid = row["id"] if row is not None else None
            con.execute("insert or replace into things (thing, key, note, lat, lon, acc, place_id, at) "
                        "values (?, ?, ?, ?, ?, ?, ?, ?)", (name, self._key(name), note_text, lat, lon, acc, pid,
                                                            self._ts()))
        return {"ok": True, "thing": name, **({"place": self.title(row)} if row is not None else {})}

    def _thing_out(self, con: sqlite3.Connection, r: sqlite3.Row) -> dict[str, Any]:
        out: dict[str, Any] = {"thing": r["thing"], "since": self._local(r["at"]).strftime("%a %Y-%m-%d %H:%M")}
        if r["note"]:
            out["note"] = r["note"]
        place = con.execute("select * from places where id = ?", (r["place_id"],)).fetchone() \
            if r["place_id"] is not None else None
        if place is not None:
            out["place"] = self.title(place)
        fix = phone_location.fresh(self.env)
        if fix is not None and r["lat"] is not None:
            d = distance_m(fix.lat, fix.lon, r["lat"], r["lon"])
            out["from_here"] = "right here" if d <= max(60.0, r["acc"] or 0) else distance_words(d)
        if r["lat"] is not None:
            out["map"] = f"https://maps.apple.com/?ll={r['lat']:.5f},{r['lon']:.5f}"
        return out

    def find_thing(self, thing: str) -> dict[str, Any]:
        key = self._key(thing or "")
        with self._db() as con:
            rows = con.execute("select * from things order by at desc").fetchall()
            hit = next((r for r in rows if r["key"] == key), None) or next(
                (r for r in rows if key and (key in r["key"] or r["key"] in key)), None)
            if hit is None:
                return {"ok": False, "reason": "eve was not told where that is",
                        "things": [r["thing"] for r in rows[:20]]}
            return {"ok": True, **self._thing_out(con, hit)}

    def list_things(self) -> dict[str, Any]:
        with self._db() as con:
            rows = con.execute("select * from things order by at desc limit 50").fetchall()
            return {"ok": True, "things": [self._thing_out(con, r) for r in rows]}

    def forget_thing(self, thing: str) -> dict[str, Any]:
        key = self._key(thing or "")
        with self._db() as con:
            n = con.execute("delete from things where key = ?", (key,)).rowcount
        return {"ok": bool(n), **({} if n else {"reason": "eve was not keeping that"})}

    # ---- upkeep ----------------------------------------------------------------------------------------------

    def prune(self) -> dict[str, int]:
        """Raw fixes past FIX_DAYS, visits past keep_days, and places no visit or thing points at any more."""
        now = self._ts()
        with self._db() as con:
            fixes = con.execute("delete from fixes where at < ?", (now - FIX_DAYS * 86400,)).rowcount
            visits = con.execute("delete from visits where coalesce(departed, arrived) < ?",
                                 (now - keep_days(self.env) * 86400,)).rowcount
            places = con.execute("delete from places where name is null and id not in (select place_id from visits) "
                                 "and id not in (select place_id from things where place_id is not null)").rowcount
        return {"fixes": fixes, "visits": visits, "places": places}

    def summary(self) -> dict[str, Any]:
        with self._db() as con:
            count = {t: con.execute(f"select count(*) from {t}").fetchone()[0]       # noqa: S608 — fixed names
                     for t in ("places", "visits", "things")}
            first = con.execute("select min(arrived) from visits").fetchone()[0]
        return {**count, "since": self._local(first).date().isoformat() if first else None}

    def forget(self) -> dict[str, int]:
        """Everything: the history, the places, the things, and the phone's latest fix."""
        with self._db() as con:
            counts = {t: con.execute(f"delete from {t}").rowcount                     # noqa: S608 — fixed names
                      for t in ("places", "visits", "fixes", "things")}
            con.execute("delete from meta")
        with contextlib.suppress(OSError):
            phone_location.state_path(self.env).unlink()
        log.info("places: forgot %s", ", ".join(f"{v} {k}" for k, v in counts.items()))
        return counts


# ---- the brain's tools -------------------------------------------------------------------------------------------

RECALL = "places_recall"
NAME = "place_name"
THING = "thing_place"
TOOL_NAMES = (RECALL, NAME, THING)

TOOLS: list[dict[str, Any]] = [
    {"type": "function", "name": RECALL, "strict": True,
     "description": ("What the owner's phone says about where they have been, from the places history on their Mac: "
                     "a day's digest (the places in order with times, time at home, at work and out, how far they "
                     "moved, new places, usual places they missed), a week's digest, the timeline of visits over "
                     "some days, the places they go with their habits (usual days, arrival time, how long), or the "
                     "known places. Answers are place names and local times. Use it for where was I, when did I "
                     "get home, how was my day or week, how often do I go to, what is my routine, and when did I "
                     "last go somewhere."),
     "parameters": {"type": "object", "additionalProperties": False,
                    "required": ["what", "start_date", "end_date"],
                    "properties": {
                        "what": {"type": "string", "enum": ["day", "week", "timeline", "habits", "places"],
                                 "description": "day: one day's digest; week: the week holding start_date; "
                                                "timeline: every visit from start_date to end_date; habits: "
                                                "patterns over the last 8 weeks; places: the known places."},
                        "start_date": {"type": "string",
                                       "description": "YYYY-MM-DD in the owner's time zone; empty for today."},
                        "end_date": {"type": "string",
                                     "description": "YYYY-MM-DD, the last day of a timeline; empty for start_date."}}}},
    {"type": "function", "name": NAME, "strict": True,
     "description": ("Give a place the name the owner uses for it, in their words (\"this is my mom's place\", "
                     "\"call this gym by its name\"). place is empty for where the owner is now, or a known place's "
                     "current name. An empty name clears it."),
     "parameters": {"type": "object", "additionalProperties": False, "required": ["name", "place"],
                    "properties": {"name": {"type": "string"}, "place": {"type": "string"}}}},
    {"type": "function", "name": THING, "strict": True,
     "description": ("Where things are: remember where the owner left or parked something (\"my car is here\", "
                     "\"I left my jacket at a friend's\"), find it again with how far it is from here and a map "
                     "link, list what is kept, or forget one. remember keeps it at where the owner is now unless "
                     "place names a known place."),
     "parameters": {"type": "object", "additionalProperties": False,
                    "required": ["action", "thing", "note", "place"],
                    "properties": {
                        "action": {"type": "string", "enum": ["remember", "find", "list", "forget"]},
                        "thing": {"type": "string", "description": "The thing, e.g. car, bike, jacket; empty for list."},
                        "note": {"type": "string", "description": "A detail the owner gave to keep with it, in "
                                                                  "their words; empty for none."},
                        "place": {"type": "string", "description": "A known place's name; empty for here."}}}},
]


def offered(environ: Optional[Mapping[str, str]] = None) -> bool:
    """The tools ride while places are on and the phone has shared a location (phone_location.offered)."""
    return enabled(environ) and phone_location.offered(environ)


def tools(environ: Optional[Mapping[str, str]] = None) -> list[dict[str, Any]]:
    return list(TOOLS) if offered(environ) else []


def _day(raw: Any, default: date) -> date:
    text = str(raw or "").strip()
    if not text:
        return default
    if not _DAY.match(text):
        raise ValueError("dates are YYYY-MM-DD")
    return date.fromisoformat(text)


def call(name: str, args: Mapping[str, Any], store: Optional[Places] = None,
         environ: Optional[Mapping[str, str]] = None) -> dict[str, Any]:
    """One tool call, synchronously (run it in a worker thread). Never raises."""
    if not enabled(environ):
        return {"ok": False, "reason": OFF_REASON}
    s = store if store is not None else Places(environ=environ)
    try:
        today = s._local(s._ts()).date()
        if name == RECALL:
            what = str(args.get("what") or "day")
            first = _day(args.get("start_date"), today)
            if what == "day":
                return {"ok": True, **s.digest(first)}
            if what == "week":
                return {"ok": True, **s.week(first)}
            if what == "timeline":
                last = _day(args.get("end_date"), first)
                if last < first or (last - first).days > 31:
                    return {"ok": False, "reason": "a timeline is at most 31 days, end not before start"}
                return {"ok": True, "visits": s.timeline(first, last)}
            if what == "habits":
                return {"ok": True, **s.patterns()}
            if what == "places":
                s._maybe_infer()
                with s._db() as con:
                    rows = con.execute("select p.*, (select count(*) from visits v where v.place_id = p.id) as c "
                                       "from places p order by c desc limit 40").fetchall()
                return {"ok": True, "places": [{**s._describe(r), "visits": r["c"],
                                                "last": s._local(r["last_seen"]).strftime("%Y-%m-%d")
                                                if r["last_seen"] else None} for r in rows]}
            return {"ok": False, "reason": "what is day, week, timeline, habits or places"}
        if name == NAME:
            return s.name_place(str(args.get("name") or ""), str(args.get("place") or ""))
        if name == THING:
            action = str(args.get("action") or "")
            thing = str(args.get("thing") or "")
            if action == "remember":
                return s.remember_thing(thing, str(args.get("note") or ""), str(args.get("place") or ""))
            if action == "find":
                return s.find_thing(thing)
            if action == "list":
                return s.list_things()
            if action == "forget":
                return s.forget_thing(thing)
            return {"ok": False, "reason": "action is remember, find, list or forget"}
        return {"ok": False, "reason": f"{name} is not a places tool"}
    except ValueError as e:
        return {"ok": False, "reason": str(e)}
    except (sqlite3.Error, OSError) as e:
        log.warning("places: %s failed (%s)", name, type(e).__name__)
        return {"ok": False, "reason": "the places history could not be read"}


def here_line(environ: Optional[Mapping[str, str]] = None) -> str:
    """The system context's line: where the owner is now, by name; '' when not known. Never raises."""
    if not enabled(environ):
        return ""
    try:
        if not state_path(environ).exists():
            return ""
        now = Places(environ=environ).here()
    except Exception as e:  # noqa: BLE001 — a prompt line is never worth a failed turn
        log.info("places: where-now unavailable (%s)", type(e).__name__)
        return ""
    if not now:
        return ""
    words = now["place"] + (f" ({now['kind']})" if now.get("kind") else "")
    return (f"Where the owner is now, from their phone: {words}" + (f", since {now['since']}" if now.get("since")
                                                                    else "") + ".")


# ---- the nightly upkeep ------------------------------------------------------------------------------------------

async def loop(shutdown: Any, interval_secs: float = 3600.0) -> None:
    """Hourly: prune what is past its days and re-infer home and work. Never raises."""
    import asyncio

    while not shutdown.is_set():
        if enabled() and state_path().exists():
            try:
                store = Places()
                pruned = await asyncio.to_thread(store.prune)
                labels = await asyncio.to_thread(store.infer_labels)
                if any(pruned.values()):
                    log.info("places: pruned %s", ", ".join(f"{v} {k}" for k, v in pruned.items()))
                log.debug("places: home %s, work %s", labels["home"] is not None, labels["work"] is not None)
            except Exception as e:  # noqa: BLE001 — upkeep tries again next hour
                log.warning("places: upkeep failed (%s)", type(e).__name__)
        try:
            await asyncio.wait_for(shutdown.wait(), interval_secs)
        except asyncio.TimeoutError:
            pass
