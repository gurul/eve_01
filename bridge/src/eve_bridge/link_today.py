"""Eve Link's home-screen widget feed: ``GET /api/link/today`` (docs/widget.md).

Owner, 2026-10-08: "make an eve widget for my phone that just tracks everything i have to do/have going, like a
command center type dashboard". One read-only call gathers what the daemon already knows, so the widget makes one
request per refresh:

* ``needs`` and ``going``: the chief's open cards (link_chat.list_jobs), the ones waiting on the owner first, then
  the ones running; and the watcher's watches (watch.Watcher) that are not paused, as more of ``going``.
* ``today``: the owner's calendar from now to the end of the local day, through the direct Google apps
  (apps_mcp.AppsBridge, ``calendar__list_events``), only when the calendar is already signed in, and the work
  calendars connected through Composio (owner, 2026-10-29: "on eve widget i dont see calendar event from work
  account"; the accounts and the read Updates uses, updates_composio.work_calendar_items). Each answer is kept for
  ``CALENDAR_KEEP_SECS``, so the widget's refreshes do not each cost a Google call per calendar.
* ``now``: what Spotify plays and whether the TV is on, from the Mini App server's own Music and TV remote cards.
* sign-ins (owner, 2026-10-26: "send a notification on the widget when u need to login"): Google refused to refresh
  its sign-in (apps_mcp.refusals), or the Google sign-in or the app's 7-day install lapses within ``SIGNIN_WARN``
  (refresh.py's dates). Each is a ``needs`` row of kind "signin", first, so every version of the widget shows it.

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
WORK_CALENDAR_SECS = 10.0    # the work calendars, one Composio call per account after listing the accounts
CALENDAR_KEEP_SECS = 300.0   # a calendar answer is reused this long
NEEDS_SHOWN = 6              # cards waiting on the owner, listed (``needs_you`` counts them all)
GOING_SHOWN = 10             # running cards and watches, listed
TODAY_SHOWN = 8              # events left today, listed
TITLE_CHARS = 80             # one line on a phone
LINE_CHARS = 60
SIGNIN_WARN = timedelta(hours=36)   # a lapse this close shows on the widget before it happens


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


def _soon(at: datetime, now: datetime) -> str:
    """When a lapse comes, as a widget line says it: "now", "in 3 h", "tomorrow 11:40 am", "Fri 5:33 pm"."""
    left = (at - now).total_seconds()
    if left <= 0:
        return "now"
    if left < 6 * 3600:
        return f"in {max(int(left // 3600), 1)} h" if left >= 3600 else f"in {max(int(left // 60), 1)} min"
    local, today = at.astimezone(), now.astimezone().date()
    day = "today" if local.date() == today else "tomorrow" if local.date() == today + timedelta(days=1) \
        else f"{local:%a}"
    return f"{day} {local:%-I:%M} {local:%p}".replace("AM", "am").replace("PM", "pm")


_app_kept: dict[str, Any] = {}      # the last build's profile expiry, by the profile's mtime (security cms is slow)


def _app_expiry() -> Optional[datetime]:
    from . import refresh

    profile = refresh.APP / "embedded.mobileprovision"
    try:
        stamp = profile.stat().st_mtime
    except OSError:
        return None
    if _app_kept.get("stamp") != stamp:
        _app_kept.update(stamp=stamp, at=refresh.app_expiry())
    return _app_kept.get("at")


def signin_part(token_path: Any, now: datetime, *, refused: Optional[float] = None,
                app_expiry: Callable[[], Optional[datetime]] = _app_expiry) -> list[dict[str, Any]]:
    """What the owner must sign in to again, as ``needs`` rows, most urgent first. ``token_path``: the Google token
    file (None when Google is not set up); ``refused``: when Google last refused a refresh (apps_mcp.refusals)."""
    from . import refresh

    rows: list[dict[str, Any]] = []
    fix = "apps login google on the Mac"
    if token_path is not None:
        lapses = refresh.google_expiry(token_path)
        signed = lapses - refresh.GOOGLE_LIFE if lapses is not None else None
        if refused is not None and (signed is None or refused > signed.timestamp()):
            rows.append({"id": "signin-google", "kind": "signin", "title": "Sign in to Google",
                         "line": "Calendar and Gmail stopped: " + fix})
        elif lapses is not None and lapses - now <= SIGNIN_WARN:
            rows.append({"id": "signin-google", "kind": "signin",
                         "title": "Google sign-in ends " + _soon(lapses, now), "line": fix})
    app = app_expiry()
    if app is not None and app - now <= SIGNIN_WARN:
        rows.append({"id": "signin-app", "kind": "signin", "title": "Eve Link stops opening " + _soon(app, now),
                     "line": "build.sh --install on the Mac"})
    for r in rows:
        r["title"], r["line"] = _clip(r["title"], TITLE_CHARS), _clip(r["line"], LINE_CHARS)
    return rows


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
    out, seen, shown = [], set(), set()
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
        row: dict[str, Any] = {"title": _clip(ev.get("summary") or "Busy", TITLE_CHARS),
                               "start": begins.timestamp(), "end": ends.timestamp(), "all_day": all_day}
        # a work meeting the owner is also invited to from the personal calendar shows once
        if key in seen or (row["title"], row["start"]) in shown:
            continue
        seen.add(key)
        shown.add((row["title"], row["start"]))
        if ev.get("location"):
            row["place"] = _clip(ev["location"], LINE_CHARS)
        out.append(row)
    out.sort(key=lambda r: (not r["all_day"], r["start"]))
    return out


def _work_bridge(inlet: Any) -> Any:
    """The Composio bridge Updates reads the work calendars through (the chat's ``_apps``), or None while Composio
    or Updates' Composio sources are off (EVE_UPDATES_COMPOSIO)."""
    from . import updates_composio

    bridge = getattr(inlet, "_apps", None) if inlet is not None else None
    return bridge if updates_composio.enabled() and updates_composio.usable(bridge) else None


def work_calendar_raw(bridge: Any, now: datetime) -> list[Any]:
    """Blocking. Today's events, from now to midnight, on each work calendar connected through Composio (the
    accounts updates_composio reads: aliased other than the owner's own), as Google's ``items`` lists. Raises
    updates_composio.SourceOff when none is connected, ReadFailed when the read failed."""
    from . import updates_composio

    reader = updates_composio.Reader(bridge)
    tomorrow = datetime.combine(now.date() + timedelta(days=1), datetime.min.time()).astimezone()
    return [reader.call("GOOGLECALENDAR_EVENTS_LIST", {
        "calendarId": "primary", "timeMin": now.isoformat(), "timeMax": tomorrow.isoformat(), "singleEvents": True,
        "orderBy": "startTime", "maxResults": 25}, account).get("items") or []
        for _alias, account in reader.accounts_for("work_calendar")]


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
                 calendar_secs: float = CALENDAR_SECS, calendar_keep_secs: float = CALENDAR_KEEP_SECS,
                 work_calendar_secs: float = WORK_CALENDAR_SECS,
                 app_expiry: Callable[[], Optional[datetime]] = _app_expiry) -> None:
        self._now, self._mono, self._app_expiry = now, mono, app_expiry
        self.source_secs, self.calendar_secs, self.calendar_keep_secs = source_secs, calendar_secs, calendar_keep_secs
        self.work_calendar_secs = work_calendar_secs
        self._calendar: Optional[tuple[float, date, Any]] = None   # (when read, which day, the answer)
        self._reading: Optional[asyncio.Task] = None                # one calendar read at a time
        self._work: Optional[tuple[float, date, Any]] = None        # the work calendars' answer, kept the same way
        self._work_reading: Optional[asyncio.Task] = None

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

    async def _work_raw(self, bridge: Any, now: datetime) -> Any:
        kept = self._work
        if kept is not None and kept[1] == now.date() and self._mono() - kept[0] < self.calendar_keep_secs:
            return kept[2]
        if self._work_reading is None or self._work_reading.done():
            self._work_reading = asyncio.ensure_future(self._read_work(bridge, now))
            self._work_reading.add_done_callback(lambda t: t.cancelled() or t.exception())
        return await asyncio.shield(self._work_reading)

    async def _read_work(self, bridge: Any, now: datetime) -> Any:
        raw = await asyncio.to_thread(work_calendar_raw, bridge, now)
        self._work = (self._mono(), now.date(), raw)
        return raw

    async def gather(self, inlet: Any, server: Any) -> dict[str, Any]:
        """Every source at once, each within its limit; what failed is left out and named in ``sources``."""
        now = self._now()
        chief = getattr(inlet, "chief", None) if inlet is not None else None
        watcher = getattr(inlet, "_watcher", None) if inlet is not None else None
        apps = getattr(inlet, "_direct", None) if inlet is not None else None
        calendar_on = apps is not None and "calendar" in (getattr(apps, "apps", None) or [])
        work = _work_bridge(inlet)
        spotify = getattr(server, "spotify", None) if server is not None else None
        tv = getattr(server, "firetv", None) if server is not None else None

        async def quick(value: Callable[[], Any]) -> Any:
            return value()

        jobs: list[tuple[str, Optional[Callable[[], Awaitable[Any]]], float]] = [
            ("jobs", (lambda: quick(lambda: jobs_part(chief))) if chief is not None else None, self.source_secs),
            ("watches", (lambda: quick(lambda: watches_part(watcher))) if watcher is not None else None,
             self.source_secs),
            ("calendar", (lambda: self._calendar_raw(apps, now)) if calendar_on else None, self.calendar_secs),
            ("work_calendar", (lambda: self._work_raw(work, now)) if work is not None else None,
             self.work_calendar_secs),
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
            except FileNotFoundError:     # updates_composio.SourceOff: no work calendar is connected
                return name, "off", None
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
        calendars = [value[k] for k in ("calendar", "work_calendar") if value[k] is not None]
        if calendars:
            out["today"] = events_part(calendars, now)[:TODAY_SHOWN]
        out["now"] = music_part(value["music"]) + tv_part(value["tv"])
        try:
            from . import apps_mcp

            token = apps.config.google_token_path if apps is not None and getattr(apps, "config", None) else None
            signin = await asyncio.wait_for(asyncio.to_thread(
                signin_part, token, now, refused=apps_mcp.refusals.get("google"), app_expiry=self._app_expiry),
                self.source_secs)
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001 — the type only; a sign-in check never fails the answer
            log.warning("link today: the sign-in check failed (%s); left out", type(e).__name__)
            signin = []
        if signin:
            out["needs"] = (signin + out["needs"])[:NEEDS_SHOWN]
            out["needs_you"] += len(signin)
        log.info("link today: %d need you (%d sign-in), %d going, %d today, %d now (%s)", out["needs_you"],
                 len(signin), len(out["going"]), len(out["today"]), len(out["now"]),
                 ", ".join(f"{k} {v}" for k, v in sources.items() if v != "ok") or "all ok")
        return out
