"""Events near the owner: concerts, games and shows by place and date, from Ticketmaster's Discovery API.

Why (owner, 2026-10-08: "this should also be using ticketmaster"): "any concerts in Seattle this weekend" went to
web_search, which timed out on a slow night. Ticketmaster answers the same question in about 300 ms (measured live
2026-10-08: ``events.json?city=Seattle&classificationName=music`` gave HTTP 200 in 287 ms, 566 events). Before this,
eve used Ticketmaster only as one watch kind (watch.py ``ticketmaster``: is one artist's show on sale yet); nothing
searched events by place and date.

One tool, ``events_search``: a city (or "city, ST"), a date range in the owner's own days, a category and words.
It returns at most MAX_EVENTS events, each trimmed to what a reply needs (the raw answer is about 9 KB an event):
name, local date and time, venue, city, on-sale status, genre, a price range when Ticketmaster lists one (it often
does not; a missing range is unknown, never free) and the ticket link.

What it shares with watch.py: the API root (``watch.TM_API``), the guarded HTTP GET (``watch.http_request``: the
address checks, no proxy, a hard deadline) and, when a Watcher runs, its rate limiter, so a search counts against
the same 5-a-second, 5000-a-day key as the watches. The key is read from ``TICKETMASTER_API_KEY`` at each call and
is never logged, returned or kept; a failure is reported by its HTTP status only, never with the URL.

``search`` never raises: no key, a timeout, a refusal and an unreadable answer are each an ``ok: False`` result
with a plain reason. The whole call is bounded by TIMEOUT_S.
"""

from __future__ import annotations

import asyncio
import json
import logging
import os
import re
import urllib.parse
from datetime import date, datetime, time, timedelta, timezone, tzinfo
from typing import Any, Callable, Mapping, Optional

from . import watch

log = logging.getLogger(__name__)

TOOL_NAME = "events_search"
TOOL_NAMES = (TOOL_NAME,)
KEY_ENV = "TICKETMASTER_API_KEY"
TIMEOUT_S = 6.0
MAX_WAIT_S = 2.0                   # a rate wait this short is waited; a longer one is said, never slept through
MAX_EVENTS = 8
# Ticketmaster's segment names, as classificationName takes them; "" searches every kind.
CATEGORIES = ("music", "sports", "arts & theatre", "film", "family", "")
OFF_REASON = "Ticketmaster is not set up on this computer (TICKETMASTER_API_KEY)"
_DAY = re.compile(r"^\d{4}-\d{2}-\d{2}$")
_STATE = re.compile(r"^[A-Za-z]{2}$")

TOOL: dict[str, Any] = {
    "type": "function", "name": TOOL_NAME, "strict": True,
    "description": ("Find concerts, sports games, theatre, comedy and other ticketed events on Ticketmaster by "
                    "place, dates, kind or name. Returns up to 8 upcoming events with venue, local date and time, "
                    "price range when listed, and the ticket link."),
    "parameters": {
        "type": "object", "additionalProperties": False,
        "required": ["city", "keyword", "category", "start_date", "end_date"],
        "properties": {
            "city": {"type": "string",
                     "description": "The city, optionally with its state code (\"Seattle\", \"Portland, OR\"); "
                                    "empty for anywhere."},
            "keyword": {"type": "string",
                        "description": "An artist, team, show, venue or genre the owner named; empty for any."},
            "category": {"type": "string", "enum": list(CATEGORIES),
                         "description": "The kind of event, or empty for every kind."},
            "start_date": {"type": "string",
                           "description": "First day, YYYY-MM-DD in the owner's time zone; empty for from now."},
            "end_date": {"type": "string",
                         "description": "Last day, YYYY-MM-DD in the owner's time zone; empty for no end."}}},
}

INSTRUCTIONS = ("Concerts, games, shows and other ticketed events are events_search: what is on in a place or on "
                "dates (\"anything on in Seattle this weekend\"), when a team plays next, whether an artist is "
                "touring or coming to a city, and tickets for any of them. A team's schedule (its next game, its "
                "next home game) and an artist's tour dates are here too: the team or artist goes in keyword, and "
                "the city stays empty unless the owner names one (for a home game, the team's own city). It reads "
                "Ticketmaster directly and answers in about a second. Work out the dates from today's date in your "
                "note.")
WATCH_HINT = (" To be told when tickets for one go on sale, that is watch_add with kind ticketmaster.")

Fetch = Callable[..., tuple[int, str]]


def available(environ: Optional[Mapping[str, str]] = None) -> bool:
    """Whether the key is set: the tool is lent only then."""
    env = os.environ if environ is None else environ
    return bool((env.get(KEY_ENV) or "").strip())


def instructions(watch_ticketmaster: bool = False) -> str:
    """The brain's line for events_search; the watch hint only when watch_add offers the ticketmaster kind (no name
    in a prompt that the turn does not offer)."""
    return INSTRUCTIONS + (WATCH_HINT if watch_ticketmaster else "")


# ---- the query --------------------------------------------------------------------------------------------------

def _utc(moment: datetime) -> str:
    """Ticketmaster's startDateTime/endDateTime form: UTC, seconds, a Z, no fraction."""
    return moment.astimezone(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def _day(raw: Any) -> Optional[date]:
    text = str(raw or "").strip()
    if not text:
        return None
    if not _DAY.match(text):
        raise ValueError(text)
    return date.fromisoformat(text)


def midnight(day: date, zone: Optional[tzinfo]) -> datetime:
    """The start of ``day`` in the owner's zone, with that day's own UTC offset. ``datetime.now().astimezone()``
    carries a fixed offset (today's), so a day across a daylight-saving change was an hour off with it (review,
    2026-10-08): a fixed offset is read as this Mac's local zone (astimezone of a naive time applies the offset in
    force on that day); a real zone (a ZoneInfo, in tests and anywhere one is passed) keeps its own rules."""
    if zone is None or isinstance(zone, timezone):
        return datetime.combine(day, time.min).astimezone()
    return datetime.combine(day, time.min, tzinfo=zone)


def query(args: Mapping[str, Any], now: datetime) -> dict[str, str]:
    """The Discovery API parameters for one search (without the key). ``now`` is aware, in the owner's zone: a
    day is that zone's midnight to midnight (each with its own offset, ``midnight``), sent in UTC. Raises
    ValueError for a date that is not YYYY-MM-DD or an end before the start."""
    zone = now.tzinfo
    first, last = _day(args.get("start_date")), _day(args.get("end_date"))
    start = now
    if first is not None:
        start = max(midnight(first, zone), now)    # "today" starts now: what began this morning is not news
    q: dict[str, str] = {"size": str(MAX_EVENTS), "sort": "date,asc", "startDateTime": _utc(start),
                         "includeTBA": "no", "includeTBD": "no"}
    if last is not None:
        end = midnight(last + timedelta(days=1), zone) - timedelta(seconds=1)
        if end < start:
            raise ValueError("end before start")
        q["endDateTime"] = _utc(end)
    city = " ".join(str(args.get("city") or "").split())
    if "," in city:
        name, _, rest = city.partition(",")
        rest = rest.strip()
        if _STATE.match(rest):
            q["stateCode"] = rest.upper()
        city = name.strip()
    if city:
        q["city"] = city[:80]
    keyword = " ".join(str(args.get("keyword") or "").split())
    if keyword:
        q["keyword"] = keyword[:120]
    category = str(args.get("category") or "").strip().lower()
    if category in CATEGORIES and category:
        q["classificationName"] = category
    return q


# ---- the answer -------------------------------------------------------------------------------------------------

def _first(items: Any) -> dict[str, Any]:
    return items[0] if isinstance(items, list) and items and isinstance(items[0], dict) else {}


def _name(raw: Any) -> str:
    return str(raw.get("name") or "") if isinstance(raw, dict) else ""


def _money(x: Any) -> str:
    try:
        v = float(x)
    except (TypeError, ValueError):
        return ""
    return f"{v:.0f}" if v == int(v) else f"{v:.2f}"


def trim(event: Mapping[str, Any]) -> dict[str, Any]:
    """One event as the brain needs it. Fields Ticketmaster left out are left out (or empty), never guessed."""
    venue = _first((event.get("_embedded") or {}).get("venues"))
    start = ((event.get("dates") or {}).get("start") or {})
    status = (((event.get("dates") or {}).get("status") or {}).get("code") or "")
    cls = _first(event.get("classifications"))
    genre = _name(cls.get("genre")) or _name(cls.get("segment"))
    out: dict[str, Any] = {
        "name": str(event.get("name") or ""),
        "date": str(start.get("localDate") or ""),
        "time": str(start.get("localTime") or "")[:5],
        "venue": _name(venue),
        "city": ", ".join(x for x in (_name(venue.get("city")), str((venue.get("state") or {}).get("stateCode")
                                                                       or "")) if x),
        "status": str(status),
        "genre": "" if genre.lower() == "undefined" else genre,
        "url": str(event.get("url") or ""),
    }
    price = _first(event.get("priceRanges"))
    lo, hi = _money(price.get("min")), _money(price.get("max"))
    if lo or hi:
        cur = str(price.get("currency") or "").upper()
        out["price"] = (f"{cur} " if cur else "") + (lo if lo == hi or not hi else f"{lo}-{hi}" if lo else hi)
    return out


def parse(payload: Any) -> dict[str, Any]:
    """The tool's result for a Discovery answer: the trimmed events and how many there are in all."""
    if not isinstance(payload, dict):
        return {"ok": False, "reason": "Ticketmaster sent an unreadable answer"}
    events = [trim(e) for e in watch._tm_events(payload)][:MAX_EVENTS]
    total = (payload.get("page") or {}).get("totalElements")
    out: dict[str, Any] = {"ok": True, "events": events}
    if isinstance(total, int) and not isinstance(total, bool):
        out["total"] = total
    if not events:
        out["note"] = "Ticketmaster lists no events for that search"
    return out


def _reason(e: Exception) -> str:
    """A failure in words, by status only: never the URL (it carries the key)."""
    status = getattr(e, "status", 0)
    if isinstance(e, (asyncio.TimeoutError, TimeoutError)):
        return "Ticketmaster did not answer in time"
    if status in (401, 403):
        return "Ticketmaster refused the key (TICKETMASTER_API_KEY)"
    if status == 429:
        return "Ticketmaster asked to slow down; try again in a minute"
    if status:
        return f"Ticketmaster answered HTTP {status}"
    if isinstance(e, watch.FetchError) and "too long" in e.reason:
        return "Ticketmaster did not answer in time"
    return "Ticketmaster could not be reached"


class Events:
    """``events_search`` for the chat brain. ``fetch`` is watch.http_request's shape (a fake in tests);
    ``limiter`` returns the Watcher's RateLimiter when one runs, so a search is counted with the watches."""

    def __init__(self, *, fetch: Optional[Fetch] = None,
                 limiter: Callable[[], Any] = lambda: None,
                 now: Callable[[], datetime] = lambda: datetime.now().astimezone(),
                 environ: Optional[Mapping[str, str]] = None, timeout_s: float = TIMEOUT_S,
                 offload: bool = True) -> None:
        self._fetch = fetch or (lambda url, **kw: watch.http_request(url, timeout=timeout_s, **kw))
        self._limiter = limiter
        self._now = now
        self._env = environ
        self._timeout = timeout_s
        self._offload = offload

    def enabled(self) -> bool:
        return available(self._env)

    def _limiter_now(self) -> Any:
        try:
            return self._limiter()
        except Exception:  # noqa: BLE001 — no limiter is no limit here
            return None

    def tools(self) -> list[dict[str, Any]]:
        return [TOOL] if self.enabled() else []

    async def handle(self, name: str, args: Mapping[str, Any]) -> dict[str, Any]:
        if name != TOOL_NAME:
            return {"ok": False, "reason": "not an events tool"}
        return await self.search(args)

    async def search(self, args: Mapping[str, Any]) -> dict[str, Any]:
        """One search. Never raises."""
        env = os.environ if self._env is None else self._env
        key = (env.get(KEY_ENV) or "").strip()
        if not key:
            return {"ok": False, "reason": OFF_REASON}
        try:
            q = query(args, self._now())
        except ValueError:
            return {"ok": False, "reason": "dates are YYYY-MM-DD, and the last day is not before the first"}
        url = f"{watch.TM_API}/events.json?" + urllib.parse.urlencode({"apikey": key, **q})
        limiter = self._limiter_now()
        if limiter is not None:
            # The watcher's limiter, honoured as the watches honour it (review, 2026-10-08): a back-off Ticketmaster
            # asked for is kept (a request now would only be refused again and lengthen it), a short wait for the
            # rate is waited, a long one is said; each search is counted with the watches' requests.
            held = _seconds(limiter, "backed_off")
            if held > 0:
                return {"ok": False, "reason": f"Ticketmaster asked to slow down; try again in {_round(held)} s"}
            wait = _seconds(limiter, "ready_in")
            if wait > MAX_WAIT_S:
                return {"ok": False, "reason": f"Ticketmaster's rate limit is spent; try again in {_round(wait)} s"}
            if wait > 0:
                await asyncio.sleep(wait)
            try:
                limiter.take(watch.TM_HOST)
            except Exception:  # noqa: BLE001 — a limiter that cannot count costs nothing here
                pass
        try:
            call = (asyncio.to_thread(self._fetch, url, headers={"Accept": "application/json"}) if self._offload
                    else _now(self._fetch, url, headers={"Accept": "application/json"}))
            _status, body = await asyncio.wait_for(call, self._timeout + 0.5)
            payload = json.loads(body)
        except asyncio.CancelledError:
            raise
        except ValueError:
            return {"ok": False, "reason": "Ticketmaster sent an unreadable answer"}
        except Exception as e:  # noqa: BLE001 — by status only, never the URL
            reason = _reason(e)
            status = getattr(e, "status", 0)
            if limiter is not None and (status in (429, 503) or (status >= 500 and getattr(e, "retry_after", None))):
                # Ticketmaster said slow down: the shared key backs off, for the watches too (as watch.py does)
                try:
                    limiter.penalize(watch.TM_HOST, getattr(e, "retry_after", None))
                except Exception:  # noqa: BLE001
                    pass
            log.info("events: search failed (%s)", reason)
            return {"ok": False, "reason": reason}
        if limiter is not None:
            try:
                limiter.clear(watch.TM_HOST)          # an answer ends a back-off, as a watch's success does
            except Exception:  # noqa: BLE001
                pass
        out = parse(payload)
        log.info("events: %d of %s events", len(out.get("events") or []), out.get("total", "?"))
        return out


def _seconds(limiter: Any, what: str) -> float:
    """One of the limiter's waits for Ticketmaster's host, in seconds; 0 when the limiter cannot say."""
    try:
        value = float(getattr(limiter, what)(watch.TM_HOST))
    except Exception:  # noqa: BLE001 — a limiter that cannot answer holds nothing back
        return 0.0
    return value if value > 0 else 0.0


def _round(secs: float) -> int:
    return max(1, int(secs + 0.999))


async def _now(fn: Fetch, *args: Any, **kwargs: Any) -> tuple[int, str]:
    """A fetch run on the loop (tests' fakes)."""
    return fn(*args, **kwargs)
