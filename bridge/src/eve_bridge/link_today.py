"""Eve Link's home-screen widget feed: ``GET /api/link/today`` (docs/widget.md).

Owner, 2026-10-08: "make an eve widget for my phone that just tracks everything i have to do/have going, like a
command center type dashboard". One read-only call gathers what the daemon already knows, so the widget makes one
request per refresh:

* ``needs`` and ``going``: the chief's open cards (link_chat.list_jobs), the ones waiting on the owner first, then
  the ones running; and the watcher's watches (watch.Watcher) that are not paused, as more of ``going``.
* ``today``: the owner's calendar from now to the end of the local day, through the direct Google apps
  (apps_mcp.AppsBridge, ``calendar__list_events``), only when the calendar is already signed in. The answer is kept
  for ``CALENDAR_KEEP_SECS``, so the widget's refreshes do not each cost a Google call per calendar.
* ``now``: what Spotify plays and whether the TV is on, from the Mini App server's own Music and TV remote cards.

Every source has its own short limit and is gathered at the same time as the others. A source that is off, slow or
broken is left out and named in ``sources`` ("ok", "off" or "failed"); it never fails the whole answer. The link
token guards it like every other ``/api/link`` path (link_chat.LinkChat.serve). Logs carry counts, never titles.
"""
from __future__ import annotations

import asyncio
import logging
import time
from datetime import date, datetime, timedelta
from typing import Any, Awaitable, Callable, Optional

log = logging.getLogger(__name__)

SOURCE_SECS = 4.0            # each quick source's own limit (the chief, the watches, Spotify, the TV)
CALENDAR_SECS = 8.0          # the calendar reads every calendar the owner sees, one request each
CALENDAR_KEEP_SECS = 300.0   # a calendar answer is reused this long
NEEDS_SHOWN = 6              # cards waiting on the owner, listed (``needs_you`` counts them all)
GOING_SHOWN = 10             # running cards and watches, listed
TODAY_SHOWN = 8              # events left today, listed
TITLE_CHARS = 80             # one line on a phone
LINE_CHARS = 60


def _clip(text: Any, limit: int) -> str:
    s = " ".join(str(text or "").split())
    return s if len(s) <= limit else s[: limit - 1].rstrip() + "…"


# ---- the sources ------------------------------------------------------------------------------------------

def jobs_part(chief: Any) -> dict[str, Any]:
    """The chief's open cards: ``needs`` (waiting on the owner) and ``going`` (the rest), as link_chat lists them."""
    from .link_chat import list_jobs

    listed = list_jobs(chief)
    needs, going = [], []
    for j in listed["jobs"]:
        if j.get("status") in ("done", "dropped"):
            continue
        row = {"id": str(j.get("id") or ""), "kind": "job", "title": _clip(j.get("title"), TITLE_CHARS),
               "line": _clip(j.get("line"), LINE_CHARS)}
        (needs if j.get("needs_you") else going).append(row)
    return {"needs_you": int(listed.get("needs_you") or 0), "needs": needs, "going": going}


def watches_part(watcher: Any) -> list[dict[str, Any]]:
    """The watches that are on, each as one row: its label and the latest reading."""
    from . import watch

    rows = []
    for w in list(getattr(watcher, "watches", []) or []):
        if w.paused:
            continue
        line = watch.now_line(w) if w.checks or w.last_value is not None else "no reading yet"
        if w.errors >= watch.TELL_AFTER_ERRORS:
            line = "can't read it"
        rows.append({"id": w.id, "kind": "watch", "title": _clip(w.label, TITLE_CHARS), "line": _clip(line, LINE_CHARS)})
    return rows


def _moment(when: Any) -> Optional[tuple[datetime, bool]]:
    """An event's start or end (Google's ``{"dateTime"}`` or ``{"date"}``) as an aware local time, and whether it
    is a whole day."""
    if not isinstance(when, dict):
        return None
    try:
        if when.get("dateTime"):
            at = datetime.fromisoformat(str(when["dateTime"]).replace("Z", "+00:00"))
            return (at if at.tzinfo else at.astimezone()), False
        if when.get("date"):
            d = date.fromisoformat(str(when["date"]))
            return datetime(d.year, d.month, d.day).astimezone(), True
    except ValueError:
        return None
    return None


def events_part(raw: Any, now: datetime) -> list[dict[str, Any]]:
    """The events in a ``calendar__list_events`` answer that have not ended and start before tomorrow, soonest
    first, as ``{"title", "start", "end", "all_day"}`` (unix seconds). Declined and cancelled ones are left out."""
    from .meet import events_in

    tomorrow = datetime.combine(now.date() + timedelta(days=1), datetime.min.time()).astimezone()
    out, seen = [], set()
    for ev in events_in(raw):
        if ev.get("status") == "cancelled":
            continue
        if any(isinstance(g, dict) and g.get("self") and g.get("responseStatus") == "declined"
               for g in ev.get("attendees") or []):
            continue
        start, end = _moment(ev.get("start")), _moment(ev.get("end"))
        if start is None:
            continue
        begins, all_day = start
        ends = end[0] if end is not None else begins + (timedelta(days=1) if all_day else timedelta(hours=1))
        if ends <= now or begins >= tomorrow:
            continue
        key = (ev.get("id"), begins)
        if key in seen:
            continue
        seen.add(key)
        row: dict[str, Any] = {"title": _clip(ev.get("summary") or "Busy", TITLE_CHARS),
                               "start": begins.timestamp(), "end": ends.timestamp(), "all_day": all_day}
        if ev.get("location"):
            row["place"] = _clip(ev["location"], LINE_CHARS)
        out.append(row)
    out.sort(key=lambda r: (not r["all_day"], r["start"]))
    return out


def music_part(state: Any) -> list[dict[str, Any]]:
    """Spotify's status answer → one row while a track plays; none while paused or idle."""
    if not isinstance(state, dict) or not state.get("ok", True) or not state.get("playing") or not state.get("track"):
        return []
    by = ", ".join(a for a in state.get("artists") or [] if isinstance(a, str))
    return [{"kind": "music", "title": _clip(state["track"], TITLE_CHARS), "line": _clip(by, LINE_CHARS)}]


def tv_part(state: Any) -> list[dict[str, Any]]:
    """The TV remote's status answer → one row while the TV is on; none in standby or unreachable."""
    if not isinstance(state, dict) or not state.get("ok") or state.get("power") not in ("on", "screensaver"):
        return []
    what = state.get("app") or ("Screensaver" if state.get("power") == "screensaver" else "On")
    return [{"kind": "tv", "title": "TV", "line": _clip(what, LINE_CHARS)}]


# ---- the gathering ----------------------------------------------------------------------------------------

class Today:
    """``GET /api/link/today``. ``now`` is the local time (a fake in tests); ``source_secs`` and
    ``calendar_secs`` are the per-source limits."""

    def __init__(self, *, now: Callable[[], datetime] = lambda: datetime.now().astimezone(),
                 mono: Callable[[], float] = time.monotonic, source_secs: float = SOURCE_SECS,
                 calendar_secs: float = CALENDAR_SECS, calendar_keep_secs: float = CALENDAR_KEEP_SECS) -> None:
        self._now, self._mono = now, mono
        self.source_secs, self.calendar_secs, self.calendar_keep_secs = source_secs, calendar_secs, calendar_keep_secs
        self._calendar: Optional[tuple[float, date, Any]] = None   # (when read, which day, the answer)
        self._reading: Optional[asyncio.Task] = None                # one calendar read at a time

    async def _calendar_raw(self, apps: Any, now: datetime) -> Any:
        kept = self._calendar
        if kept is not None and kept[1] == now.date() and self._mono() - kept[0] < self.calendar_keep_secs:
            return kept[2]
        if self._reading is None or self._reading.done():
            self._reading = asyncio.ensure_future(self._read_calendar(apps, now))
            self._reading.add_done_callback(lambda t: t.cancelled() or t.exception())   # never "not retrieved"
        # shielded: a read that outlasts this request still finishes and is kept for the next refresh
        return await asyncio.shield(self._reading)

    async def _read_calendar(self, apps: Any, now: datetime) -> Any:
        tomorrow = datetime.combine(now.date() + timedelta(days=1), datetime.min.time()).astimezone()
        args = {"startTime": now.isoformat(), "endTime": tomorrow.isoformat(), "orderBy": "startTime", "pageSize": 25}
        raw = await asyncio.to_thread(apps.execute, "calendar__list_events", args)
        if not isinstance(raw, dict) or raw.get("ok") is False:
            raise RuntimeError("the calendar call failed")
        self._calendar = (self._mono(), now.date(), raw)
        return raw

    async def gather(self, inlet: Any, server: Any) -> dict[str, Any]:
        """Every source at once, each within its limit; what failed is left out and named in ``sources``."""
        now = self._now()
        chief = getattr(inlet, "chief", None) if inlet is not None else None
        watcher = getattr(inlet, "_watcher", None) if inlet is not None else None
        apps = getattr(inlet, "_direct", None) if inlet is not None else None
        calendar_on = apps is not None and "calendar" in (getattr(apps, "apps", None) or [])
        spotify = getattr(server, "spotify", None) if server is not None else None
        tv = getattr(server, "firetv", None) if server is not None else None

        async def quick(value: Callable[[], Any]) -> Any:
            return value()

        jobs: list[tuple[str, Optional[Callable[[], Awaitable[Any]]], float]] = [
            ("jobs", (lambda: quick(lambda: jobs_part(chief))) if chief is not None else None, self.source_secs),
            ("watches", (lambda: quick(lambda: watches_part(watcher))) if watcher is not None else None,
             self.source_secs),
            ("calendar", (lambda: self._calendar_raw(apps, now)) if calendar_on else None, self.calendar_secs),
            ("music", (lambda: spotify.panel({"action": "status"})) if spotify is not None else None,
             self.source_secs),
            ("tv", (lambda: tv.panel({"action": "status"})) if tv is not None else None, self.source_secs),
        ]

        async def one(name: str, make: Optional[Callable[[], Awaitable[Any]]], secs: float) -> tuple[str, str, Any]:
            if make is None:
                return name, "off", None
            try:
                return name, "ok", await asyncio.wait_for(make(), secs)
            except asyncio.CancelledError:
                raise
            except asyncio.TimeoutError:
                log.info("link today: %s took longer than %.0f s; left out", name, secs)
                return name, "failed", None
            except Exception as e:  # noqa: BLE001 — the type only; a source never fails the answer
                log.warning("link today: %s failed (%s); left out", name, type(e).__name__)
                return name, "failed", None

        got = await asyncio.gather(*(one(n, m, s) for n, m, s in jobs))
        sources = {name: state for name, state, _ in got}
        value = {name: v for name, _, v in got}
        out: dict[str, Any] = {"at": now.timestamp(), "needs_you": 0, "needs": [], "going": [], "today": [],
                               "now": [], "sources": sources}
        if value["jobs"] is not None:
            out["needs_you"] = value["jobs"]["needs_you"]
            out["needs"] = value["jobs"]["needs"][:NEEDS_SHOWN]
            out["going"] = value["jobs"]["going"]
        if value["watches"] is not None:
            out["going"] = out["going"] + value["watches"]
        out["going"] = out["going"][:GOING_SHOWN]
        if value["calendar"] is not None:
            out["today"] = events_part(value["calendar"], now)[:TODAY_SHOWN]
        out["now"] = music_part(value["music"]) + tv_part(value["tv"])
        log.info("link today: %d need you, %d going, %d today, %d now (%s)", out["needs_you"], len(out["going"]),
                 len(out["today"]), len(out["now"]),
                 ", ".join(f"{k} {v}" for k, v in sources.items() if v != "ok") or "all ok")
        return out
