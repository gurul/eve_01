"""The phone's live location: the Eve Link app shares where the iPhone is, and eve can read it.

Owner, 2026-10-12: "give access to live phone location through app". The app posts a fix to
``POST /api/link/location`` (link_chat.py, the link token, any Origin refused) and this module keeps it; eve's
brain reads it with one read-only tool, ``phone_location``, in the chat, the app chat and out loud.

**What is kept.** Only the latest fix, in one small file (``DEFAULT_FILE``, beside the chief's folder and the
link token; ``EVE_PHONE_LOCATION_FILE`` moves it). A new fix replaces it with a temp file in the same folder and
``os.replace`` (atomic), owner-only (0600). There is no history: a fix older than (or as old as) the one stored is
ignored, so a late retry never moves the owner back. Coordinates and the place are never logged and never sent
anywhere by this module: a log line says "location updated" with the fix's age and accuracy only. The tool's
answer goes to the model that asked for it, like any tool result, and is not written to the transcript
(telegram.TRANSCRIBED_TOOLS, voice_agent.ANSWER_TOOLS do not list it). quick_answers.py sends a fresh fix's
coordinates, rounded to two decimals (about 1 km), to Open-Meteo for "what's the weather", as it sends
``EVE_WEATHER_LAT``/``LON``.

**The fix.** ``{"lat", "lon", "accuracy_m", "at", "place", "speed_mps"}``: degrees, the horizontal accuracy in
meters, when the phone took it (ISO 8601 with a zone), the phone's reverse-geocoded name for it (a town or a
street, or null) and the speed in meters per second (null when unknown; iOS's -1, "invalid", is kept as null).
``parse`` refuses anything else with a reason (the route's 400).

**The tool.** ``phone_location`` (no arguments) answers ``{"place", "lat", "lon", "accuracy_m", "age_min",
"stale"}`` (``stale`` past ``STALE_MIN`` minutes) or ``{"error": NO_FIX}``. It is offered while the switch is on
and a fix has been stored (``offered``): a Mac whose phone never shared its location sends every prompt byte for
byte as before, the way the camera's tools ride only while there is a camera.

**The switch.** ``EVE_PHONE_LOCATION`` (on unless 0/false/no/off): off, the route answers 403 and the tool is
neither offered nor answered.
"""

from __future__ import annotations

import contextlib
import json
import logging
import math
import os
import re
import tempfile
import threading
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Mapping, Optional

log = logging.getLogger(__name__)

SWITCH = "EVE_PHONE_LOCATION"
FILE_ENV = "EVE_PHONE_LOCATION_FILE"
DEFAULT_FILE = "~/.config/eve-bridge/phone-location.json"
STALE_MIN = 60                          # a fix older than this many minutes is stale
FUTURE_SECS = 300.0                     # a fix stamped further ahead than this (clock skew) is refused
MAX_ACCURACY_M = 100_000.0              # 100 km: past this a "fix" says nothing about where the phone is
MAX_SPEED_MPS = 350.0                   # faster than an airliner is garbage
MAX_PLACE_CHARS = 120
COORD_DECIMALS = 5                      # about a meter: what the tool gives the model
NO_FIX = "no location from the phone yet"
OFF_REASON = "Phone location is off on this Mac (EVE_PHONE_LOCATION=0)."
TOOL_NAME = "phone_location"

_CONTROL = re.compile(r"[\x00-\x1f\x7f-\x9f  ]")

TOOL: dict[str, Any] = {
    "type": "function", "name": TOOL_NAME, "strict": True,
    "description": ("Where the owner is right now, from their iPhone's GPS, shared by the Eve Link app: the "
                    "place's name (a town or a street, or null), latitude and longitude, the accuracy in meters, "
                    "how many minutes ago the phone took it (age_min) and whether that is over an hour old "
                    "(stale). Use it when the owner asks where they are, what is near them, how far or how long "
                    "it is from here to somewhere, or the time or weather where they are, and they name no other "
                    "place. It is their live position, newer than any home location you were given. When stale "
                    "is true, say how old it is. An error means the phone has not shared a location yet."),
    "parameters": {"type": "object", "additionalProperties": False, "required": [], "properties": {}},
}
TOOLS: list[dict[str, Any]] = [TOOL]
TOOL_NAMES = (TOOL_NAME,)

_lock = threading.Lock()                # one writer at a time in this process: the compare and the replace


def enabled(environ: Optional[Mapping[str, str]] = None) -> bool:
    """EVE_PHONE_LOCATION: on unless 0, false, no or off."""
    env = os.environ if environ is None else environ
    return (env.get(SWITCH) or "").strip().lower() not in ("0", "false", "no", "off")


def state_path(environ: Optional[Mapping[str, str]] = None) -> Path:
    """The one file the latest fix lives in: EVE_PHONE_LOCATION_FILE (from ``environ``, then the process's), else
    DEFAULT_FILE."""
    env = os.environ if environ is None else environ
    raw = (env.get(FILE_ENV) or os.environ.get(FILE_ENV) or DEFAULT_FILE).strip()
    return Path(raw).expanduser()


@dataclass(frozen=True)
class Fix:
    lat: float
    lon: float
    accuracy_m: float
    at: datetime                        # aware, UTC
    place: Optional[str] = None
    speed_mps: Optional[float] = None

    def as_json(self) -> dict[str, Any]:
        return {"lat": self.lat, "lon": self.lon, "accuracy_m": self.accuracy_m,
                "at": self.at.isoformat(), "place": self.place, "speed_mps": self.speed_mps}

    def age_secs(self, now: Optional[datetime] = None) -> float:
        now = now if now is not None else datetime.now(timezone.utc)
        return max(0.0, (now - self.at).total_seconds())


class BadFix(ValueError):
    """A body that is not a fix; its message is the route's 400 reason (no value from the body in it)."""


def _number(body: Mapping[str, Any], key: str, low: float, high: float, *, optional: bool = False) -> Optional[float]:
    value = body.get(key)
    if value is None and optional:
        return None
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise BadFix(f"{key} must be a number")
    try:
        value = float(value)
    except OverflowError:                         # an integer too big for a float (JSON allows it): out of range
        raise BadFix(f"{key} must be from {low:g} to {high:g}") from None
    if not math.isfinite(value) or not low <= value <= high:
        raise BadFix(f"{key} must be from {low:g} to {high:g}")
    return value


def _place(raw: Any) -> Optional[str]:
    if raw is None:
        return None
    if not isinstance(raw, str):
        raise BadFix("place must be a string or null")
    text = " ".join(_CONTROL.sub(" ", raw).split())
    if len(text) > MAX_PLACE_CHARS:
        raise BadFix(f"place is at most {MAX_PLACE_CHARS} characters")
    return text or None


def _when(raw: Any) -> datetime:
    if not isinstance(raw, str) or not 10 <= len(raw) <= 40:
        raise BadFix("at must be an ISO 8601 time with a zone")
    text = raw.strip()
    if text.endswith(("Z", "z")):
        text = text[:-1] + "+00:00"
    try:
        at = datetime.fromisoformat(text)
    except ValueError:
        raise BadFix("at must be an ISO 8601 time with a zone") from None
    if at.tzinfo is None or at.utcoffset() is None:
        raise BadFix("at must be an ISO 8601 time with a zone")
    return at.astimezone(timezone.utc)


def parse(body: Any, now: Optional[datetime] = None) -> Fix:
    """The app's body as a Fix, or BadFix with the reason. Keys other than the six are ignored."""
    if not isinstance(body, dict):
        raise BadFix("the body must be a JSON object")
    lat = _number(body, "lat", -90.0, 90.0)
    lon = _number(body, "lon", -180.0, 180.0)
    accuracy = _number(body, "accuracy_m", 0.0, MAX_ACCURACY_M)
    speed_raw = body.get("speed_mps")
    if isinstance(speed_raw, (int, float)) and not isinstance(speed_raw, bool) and speed_raw < 0:
        speed: Optional[float] = None       # iOS's "invalid" (-1): no speed, not garbage
    else:
        speed = _number(body, "speed_mps", 0.0, MAX_SPEED_MPS, optional=True)
    at = _when(body.get("at"))
    now = now if now is not None else datetime.now(timezone.utc)
    if (at - now).total_seconds() > FUTURE_SECS:
        raise BadFix("at is in the future")
    assert lat is not None and lon is not None and accuracy is not None
    return Fix(lat=lat, lon=lon, accuracy_m=accuracy, at=at, place=_place(body.get("place")), speed_mps=speed)


def load(path: Optional[Path] = None) -> Optional[Fix]:
    """The stored fix, or None (no file, or one that is not a fix: never raises)."""
    path = path if path is not None else state_path()
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
        fix = parse(raw, now=datetime.max.replace(tzinfo=timezone.utc))   # what was stored is never "future"
    except (OSError, ValueError):
        return None
    return fix


def store(fix: Fix, path: Optional[Path] = None) -> bool:
    """Keep ``fix`` as the only one, atomically and owner-only. False (nothing written) when the stored fix is as
    new or newer."""
    path = path if path is not None else state_path()
    with _lock:
        old = load(path)
        if old is not None and fix.at <= old.at:
            return False
        if not path.parent.exists():
            path.parent.mkdir(parents=True, mode=0o700, exist_ok=True)
        fd, tmp = tempfile.mkstemp(prefix=".phone-location-", suffix=".tmp", dir=str(path.parent))
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as fh:
                os.fchmod(fh.fileno(), 0o600)
                json.dump(fix.as_json(), fh)
                fh.flush()
                os.fsync(fh.fileno())
            os.replace(tmp, path)
        except BaseException:
            with contextlib.suppress(OSError):
                os.unlink(tmp)
            raise
        os.chmod(path, 0o600)
    return True


def fresh(environ: Optional[Mapping[str, str]] = None, *, max_age_min: float = STALE_MIN,
          now: Optional[datetime] = None) -> Optional[Fix]:
    """The stored fix while the switch is on and it is at most ``max_age_min`` old; None otherwise."""
    if not enabled(environ):
        return None
    fix = load(state_path(environ))
    if fix is None or fix.age_secs(now) > max_age_min * 60:
        return None
    return fix


def offered(environ: Optional[Mapping[str, str]] = None) -> bool:
    """Whether a turn offers the tool: the switch is on and the phone has shared a fix (any age)."""
    return enabled(environ) and state_path(environ).is_file()


def tools(environ: Optional[Mapping[str, str]] = None) -> list[dict[str, Any]]:
    """The tool for this turn, or [] (``offered``)."""
    return list(TOOLS) if offered(environ) else []


def reading(environ: Optional[Mapping[str, str]] = None, now: Optional[datetime] = None) -> dict[str, Any]:
    """The tool's answer."""
    fix = load(state_path(environ))
    if fix is None:
        return {"error": NO_FIX}
    age = fix.age_secs(now)
    return {"place": fix.place, "lat": round(fix.lat, COORD_DECIMALS), "lon": round(fix.lon, COORD_DECIMALS),
            "accuracy_m": round(fix.accuracy_m), "age_min": int(age // 60), "stale": age > STALE_MIN * 60}


async def handle(name: str, args: Mapping[str, Any], environ: Optional[Mapping[str, str]] = None) -> dict[str, Any]:
    """One call of the tool (it takes no arguments). Off: the reason, never the stored fix."""
    if name != TOOL_NAME:
        return {"ok": False, "reason": f"{name} is not the phone's location tool"}
    if not enabled(environ):
        return {"ok": False, "reason": OFF_REASON}
    return reading(environ)


def describe(fix: Fix, now: Optional[datetime] = None) -> str:
    """A log line's words for a fix: its age and accuracy, never where it is."""
    return f"age {round(fix.age_secs(now))} s, accuracy {round(fix.accuracy_m)} m"
