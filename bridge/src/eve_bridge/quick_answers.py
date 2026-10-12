"""The standard details, answered by code: no model turn and no web search.

The owner asked on 2026-09-26: "route in standard details like time weather and things like that manually … i
dont need it searching unnecessary stuff … look for other things you can feed in as well". A question that is
only a standard question needs no judgement, so code answers it at once:

======================  =================================================================  ==================
Ask                     Examples                                                           Source
======================  =================================================================  ==================
time                    "what time is it", "what time is it in Tokyo"                      the clock (+ geocoder for a place)
date                    "what's the date", "what day is it"                                the clock
weather                 "what's the weather", "weather tomorrow", "weather in Paris",      Open-Meteo forecast
                        "will it rain today", "how cold is it", "do I need an umbrella"
sun                     "when is sunset", "what time is sunrise tomorrow"                  Open-Meteo forecast
air                     "what's the air quality", "air quality in Delhi"                   Open-Meteo air quality
math                    "what's 17 times 23", "15 percent of 80", "2 to the power of 10"   arithmetic (no eval)
convert                 "convert 5 km to miles", "72 fahrenheit in celsius",               a unit table
                        "how many ounces in a pound"
countdown               "how many days until christmas"                                    the calendar
battery                 "what's my battery", "how much battery do I have"                  the Mac's pmset
======================  =================================================================  ==================

A question is answered here only when it is **only** that question (``QuickAnswers.match``): "what time is my
meeting", "should I bike given the weather" and "what's 17 times my rent" go to the model, which has the
calendar, the search and the judgement. When a source fails, ``answer`` returns None and the model answers as
before, so a failure here costs nothing. Open-Meteo is free and needs no key.

Where "here" is: the phone's own location while it is fresh (phone_location.py: the Eve Link app shared a fix at
most ``PHONE_FRESH_MIN`` minutes ago; its coordinates go to Open-Meteo rounded to ``PHONE_DECIMALS`` decimals, about
a kilometer), else ``EVE_WEATHER_PLACE`` (a place name, looked up once with Open-Meteo's geocoder), or
``EVE_WEATHER_LAT`` and ``EVE_WEATHER_LON``. Without any of them, questions about "here" (weather, sun, air)
go to the model. The time zone is ``EVE_TIMEZONE`` (an IANA name), as system_context.py reads it, else the
Mac's.

Coordinates never reach the log (``CoordinateFilter``): httpx logs each request line at INFO with its query string,
and the daemon logs at INFO, so every Open-Meteo call would write where the phone was, a location history in a
plain file. The filter on the "httpx" logger replaces the latitude and longitude values in any Open-Meteo URL.
"""
from __future__ import annotations

import ast
import asyncio
import datetime as dt
import logging
import math
import operator
import os
import re
from dataclasses import dataclass, field
from typing import Any, Awaitable, Callable, Mapping, Optional
from zoneinfo import ZoneInfo, ZoneInfoNotFoundError

log = logging.getLogger(__name__)

FORECAST_URL = "https://api.open-meteo.com/v1/forecast"
AIR_URL = "https://air-quality-api.open-meteo.com/v1/air-quality"
GEOCODE_URL = "https://geocoding-api.open-meteo.com/v1/search"
TIMEOUT_SECS = 5.0
PHONE_FRESH_MIN = 60                     # phone_location.STALE_MIN: an older fix is not "here" any more
PHONE_DECIMALS = 2                       # what of a phone fix Open-Meteo is sent: about a kilometer
PHONE_PLACE = "where you are"            # a phone fix with no place name
HIDDEN = "<hidden>"                      # what the log shows for a coordinate
_COORD = re.compile(r"\b((?:latitude|longitude)=)[^&\s\"']+")


class CoordinateFilter(logging.Filter):
    """Rewrites a record that quotes an Open-Meteo URL so its latitude/longitude values read ``HIDDEN``. httpx logs
    through the one logger "httpx" (httpcore's request logs carry no URL), so the filter sits there."""

    def filter(self, record: logging.LogRecord) -> bool:
        try:
            message = record.getMessage()
        except Exception:  # noqa: BLE001 — a record that cannot format is the logging module's to report
            return True
        if "open-meteo.com" in message and _COORD.search(message):
            record.msg, record.args = _COORD.sub(r"\1" + HIDDEN, message), None
        return True


def hide_coordinates() -> None:
    """Install CoordinateFilter on the "httpx" logger, once (import does it)."""
    logger = logging.getLogger("httpx")
    if not any(isinstance(f, CoordinateFilter) for f in logger.filters):
        logger.addFilter(CoordinateFilter())


hide_coordinates()


@dataclass(frozen=True)
class Ask:
    kind: str
    place: str = ""                      # "" = here
    day: int = 0                         # 0 = today, 1 = tomorrow
    args: dict[str, Any] = field(default_factory=dict)


# ---- understanding the question ------------------------------------------------------------------

_LEAD = re.compile(r"^(?:(?:hey|hi|ok|okay|yo)\s+)?eve\b[\s,:]*")
_POLITE = re.compile(r"^(?:(?:can|could) you (?:tell me|check)|do you know|tell me|please tell me|please)\s+")
_TAIL = re.compile(r"\s+(?:please|eve|thanks|thank you)$")
_PLACE = r"(?P<place>[a-z][a-z .'\-]{1,40}?)"
_WHEN = r"(?P<when> today| now| right now| tomorrow| outside| out)?"


def normalize(text: str) -> str:
    t = text.lower().replace("’", "'").strip()
    t = _LEAD.sub("", t)
    t = re.sub(r"\s+", " ", t).strip(" ?!.,")
    t = _POLITE.sub("", t)
    t = _TAIL.sub("", t)
    return t.strip(" ?!.,")


def _day(when: Optional[str]) -> int:
    return 1 if when and "tomorrow" in when else 0


_TIME = [
    re.compile(r"(?:what(?:'s| is)? (?:the )?time(?: is it)?|what time is it|what time it is|the time|time)"
               r"(?: now| right now)?(?: in " + _PLACE + r")?(?: now| right now)?"),
]
_DATE = re.compile(
    r"what(?:'s| is)? (?:the )?(?:date|day)(?: today)?(?: is it)?|what(?:'s| is)? today(?:'s date)?"
    r"|what day is (?:it|today)(?: today)?|what date is (?:it|today)|today's date|the date|date")
_WEATHER = [
    re.compile(r"(?:(?:what(?:'s| is)?|how(?:'s| is)?) (?:the )?)?weather(?: like| going to be like| forecast)?"
               + _WHEN + r"(?: in " + _PLACE + r")?" + _WHEN.replace("when", "when2")),
    re.compile(r"(?:what(?:'s| is)?|how(?:'s| is)?) it (?:like )?(?:outside|out)"),
    re.compile(r"(?:is it|will it|is it going to|is it gonna) (?:rain|raining|snow|snowing)" + _WHEN
               + r"(?: in " + _PLACE + r")?"),
    re.compile(r"(?:what(?:'s| is)? )?(?:the )?temperature(?: outside)?" + _WHEN + r"(?: in " + _PLACE + r")?"),
    re.compile(r"how (?:hot|cold|warm) is it(?: outside)?" + _WHEN + r"(?: in " + _PLACE + r")?"),
    re.compile(r"do i need (?:an umbrella|a jacket|a coat)" + _WHEN),
]
_SUN = re.compile(r"(?:when(?:'s| is)?|what time(?:'s| is)?) (?:the )?(?P<which>sunrise|sunset)" + _WHEN
                  + r"|when does the sun (?P<verb>rise|set)" + _WHEN.replace("when", "when2"))
_AIR = re.compile(r"(?:(?:what(?:'s| is)?|how(?:'s| is)?) (?:the )?)?(?:air quality|aqi)" + _WHEN
                  + r"(?: in " + _PLACE + r")?")
_BATTERY = re.compile(r"(?:what(?:'s| is)? )?(?:my |the )?(?:mac(?:book)?'?s? |laptop'?s? |computer'?s? )?battery"
                      r"(?: level| percentage)?(?: at)?|how much battery (?:do i have|is left|left)(?: left)?"
                      r"|how(?:'s| is) (?:my |the )?battery")
_COUNTDOWN = re.compile(r"(?:how many days|how long) (?:is it )?(?:until|till|til|to|before) (?P<what>[a-z' ]{3,30})")


def match(text: str) -> Optional[Ask]:
    """The standard question these words are, or None when they are anything more."""
    t = normalize(text)
    if not t or len(t) > 90:
        return None
    for p in _TIME:
        m = p.fullmatch(t)
        if m:
            return Ask("time", place=(m.group("place") or "").strip())
    if _DATE.fullmatch(t):
        return Ask("date")
    for p in _WEATHER:
        m = p.fullmatch(t)
        if m:
            g = m.groupdict()
            when = g.get("when") or g.get("when2")
            return Ask("weather", place=(g.get("place") or "").strip(), day=_day(when))
    m = _SUN.fullmatch(t)
    if m:
        which = m.group("which") or ("sunrise" if m.group("verb") == "rise" else "sunset")
        return Ask("sun", day=_day(m.group("when") or m.group("when2")), args={"which": which})
    m = _AIR.fullmatch(t)
    if m:
        return Ask("air", place=(m.group("place") or "").strip())
    if _BATTERY.fullmatch(t):
        return Ask("battery")
    m = _COUNTDOWN.fullmatch(t)
    if m and holiday(m.group("what").strip(), dt.date(2000, 1, 1)) is not None:
        return Ask("countdown", args={"what": m.group("what").strip()})
    conv = conversion(t)
    if conv is not None:
        return Ask("convert", args=conv)
    expr = arithmetic(t)
    if expr is not None:
        return Ask("math", args=expr)
    return None


# ---- math: a small evaluator, never eval() -------------------------------------------------------

_MATH_WORDS = [
    (r"\bmultiplied by\b", "*"), (r"\btimes\b", "*"), (r"(?<=\d)\s*x\s*(?=\d)", "*"), (r"×", "*"),
    (r"\bdivided by\b", "/"), (r"\bover\b", "/"), (r"÷", "/"),
    (r"\bplus\b", "+"), (r"\bminus\b", "-"),
    (r"\bto the power of\b", "**"), (r"\braised to\b", "**"), (r"\^", "**"),
    (r"\bsquared\b", "**2"), (r"\bcubed\b", "**3"),
]
_OPS: dict[type, Callable[[Any, Any], Any]] = {
    ast.Add: operator.add, ast.Sub: operator.sub, ast.Mult: operator.mul, ast.Div: operator.truediv,
    ast.Pow: operator.pow,
}
MAX_EXPONENT = 64


def _eval(node: ast.AST) -> float:
    if isinstance(node, ast.Constant) and isinstance(node.value, (int, float)) and not isinstance(node.value, bool):
        return node.value
    if isinstance(node, ast.UnaryOp) and isinstance(node.op, (ast.USub, ast.UAdd)):
        v = _eval(node.operand)
        return -v if isinstance(node.op, ast.USub) else v
    if isinstance(node, ast.BinOp) and type(node.op) in _OPS:
        left, right = _eval(node.left), _eval(node.right)
        if isinstance(node.op, ast.Pow) and (abs(right) > MAX_EXPONENT or abs(left) > 1e6):
            raise ValueError("too big")
        return _OPS[type(node.op)](left, right)
    raise ValueError("not arithmetic")


def arithmetic(t: str) -> Optional[dict[str, Any]]:
    """{"said": the question as asked, "value": the result} for words that are only a sum."""
    body = re.sub(r"^(?:what(?:'s| is)|how much is|calculate|compute|solve)\s+", "", t)
    body = re.sub(r"\s+(?:equals?|is)$", "", body)
    pct = re.fullmatch(r"(-?\d+(?:\.\d+)?) ?(?:%|percent) of (-?\d+(?:\.\d+)?)", body)
    if pct:
        a, b = float(pct.group(1)), float(pct.group(2))
        return {"said": f"{_num(a)} percent of {_num(b)}", "value": a / 100 * b}
    root = re.fullmatch(r"(?:the )?square root of (\d+(?:\.\d+)?)", body)
    if root:
        a = float(root.group(1))
        return {"said": f"the square root of {_num(a)}", "value": math.sqrt(a)}
    if re.search(r"\d/\d", body):
        return None                       # "9/11", "24/7": a date or a phrase, not a division
    expr = body
    for pattern, sym in _MATH_WORDS:
        expr = re.sub(pattern, f" {sym} ", expr)
    expr = re.sub(r"(?<=\d),(?=\d{3})", "", expr)                      # 1,000
    if not re.fullmatch(r"[\d\s.+\-*/()]+", expr) or not re.search(r"\d\s*(?:\*\*|[+\-*/])\s*[\d(]", expr):
        return None
    try:
        value = _eval(ast.parse(expr.strip(), mode="eval").body)
    except (SyntaxError, ValueError, ZeroDivisionError, OverflowError, RecursionError):
        return None
    if not math.isfinite(value):
        return None
    return {"said": body, "value": value}


def _num(v: float) -> str:
    if abs(v - round(v)) < 1e-9 and abs(v) < 1e15:
        return f"{int(round(v)):,}"
    return f"{v:,.4f}".rstrip("0").rstrip(".")


# ---- conversions ---------------------------------------------------------------------------------

# name -> (dimension, factor to the base unit). Base: metre, gram, millilitre, km/h.
UNITS: dict[str, tuple[str, float]] = {}


def _unit(dim: str, factor: float, *names: str) -> None:
    for n in names:
        UNITS[n] = (dim, factor)


_unit("length", 0.001, "mm", "millimeter", "millimeters", "millimetre", "millimetres")
_unit("length", 0.01, "cm", "centimeter", "centimeters", "centimetre", "centimetres")
_unit("length", 1.0, "m", "meter", "meters", "metre", "metres")
_unit("length", 1000.0, "km", "kilometer", "kilometers", "kilometre", "kilometres", "k")
_unit("length", 0.0254, "in", "inch", "inches")
_unit("length", 0.3048, "ft", "foot", "feet")
_unit("length", 0.9144, "yd", "yard", "yards")
_unit("length", 1609.344, "mi", "mile", "miles")
_unit("mass", 1.0, "g", "gram", "grams")
_unit("mass", 1000.0, "kg", "kilo", "kilos", "kilogram", "kilograms")
_unit("mass", 28.349523125, "oz", "ounce", "ounces")
_unit("mass", 453.59237, "lb", "lbs", "pound", "pounds")
_unit("mass", 6350.29318, "stone", "stones")
_unit("volume", 1.0, "ml", "milliliter", "milliliters", "millilitre", "millilitres")
_unit("volume", 1000.0, "l", "liter", "liters", "litre", "litres")
_unit("volume", 4.92892159375, "tsp", "teaspoon", "teaspoons")
_unit("volume", 14.78676478125, "tbsp", "tablespoon", "tablespoons")
_unit("volume", 29.5735295625, "fl oz", "fluid ounce", "fluid ounces")
_unit("volume", 236.5882365, "cup", "cups")
_unit("volume", 473.176473, "pint", "pints")
_unit("volume", 946.352946, "quart", "quarts")
_unit("volume", 3785.411784, "gallon", "gallons")
_unit("speed", 1.0, "kph", "km/h", "kmh", "kilometers per hour", "kilometres per hour")
_unit("speed", 1.609344, "mph", "miles per hour")
_TEMPS = {"f": "F", "fahrenheit": "F", "degrees fahrenheit": "F", "degrees f": "F",
          "c": "C", "celsius": "C", "centigrade": "C", "degrees celsius": "C", "degrees c": "C"}
_UNIT = r"(?P<{}>[a-z/ ]{{1,22}}?)"


def _convert(value: float, src: str, dst: str) -> Optional[float]:
    if src in _TEMPS and dst in _TEMPS:
        a, b = _TEMPS[src], _TEMPS[dst]
        if a == b:
            return value
        return value * 9 / 5 + 32 if a == "C" else (value - 32) * 5 / 9
    s, d = UNITS.get(src), UNITS.get(dst)
    if s is None or d is None or s[0] != d[0]:
        return None
    return value * s[1] / d[1]


def conversion(t: str) -> Optional[dict[str, Any]]:
    """{"value", "src", "dst", "result"} for words that are only a unit conversion."""
    body = re.sub(r"^(?:convert|what(?:'s| is)|how much is|how many is)\s+", "", t)
    m = (re.fullmatch(r"(?P<n>-?\d+(?:\.\d+)?) ?" + _UNIT.format("src") + r" (?:to|in|into|in to) "
                      + _UNIT.format("dst"), body)
         or re.fullmatch(r"how many " + _UNIT.format("dst") + r" (?:are )?(?:in|is|are in) (?:a |an |one )?"
                         r"(?:(?P<n>-?\d+(?:\.\d+)?) ?)?" + _UNIT.format("src"), t))
    if not m:
        return None
    src, dst = m.group("src").strip(), m.group("dst").strip()
    value = float(m.group("n")) if m.group("n") else 1.0
    result = _convert(value, src, dst)
    if result is None:
        return None
    return {"value": value, "src": src, "dst": dst, "result": result}


# ---- countdowns ----------------------------------------------------------------------------------

def _nth_weekday(year: int, month: int, weekday: int, n: int) -> dt.date:
    first = dt.date(year, month, 1)
    return first + dt.timedelta(days=(weekday - first.weekday()) % 7 + 7 * (n - 1))


HOLIDAYS: dict[str, Callable[[int], dt.date]] = {
    "christmas": lambda y: dt.date(y, 12, 25),
    "christmas eve": lambda y: dt.date(y, 12, 24),
    "new year": lambda y: dt.date(y, 1, 1),
    "new year's": lambda y: dt.date(y, 1, 1),
    "new years": lambda y: dt.date(y, 1, 1),
    "new year's day": lambda y: dt.date(y, 1, 1),
    "new year's eve": lambda y: dt.date(y, 12, 31),
    "halloween": lambda y: dt.date(y, 10, 31),
    "valentine's day": lambda y: dt.date(y, 2, 14),
    "valentines day": lambda y: dt.date(y, 2, 14),
    "the fourth of july": lambda y: dt.date(y, 7, 4),
    "fourth of july": lambda y: dt.date(y, 7, 4),
    "july fourth": lambda y: dt.date(y, 7, 4),
    "independence day": lambda y: dt.date(y, 7, 4),
    "thanksgiving": lambda y: _nth_weekday(y, 11, 3, 4),              # the fourth Thursday of November
}


def holiday(what: str, today: dt.date) -> Optional[tuple[str, dt.date]]:
    """(its name, its next date on or after today), for a holiday this module knows."""
    key = re.sub(r"\s+", " ", what.strip().lower())
    key = re.sub(r"^(?:next )", "", key)
    f = HOLIDAYS.get(key)
    if f is None:
        return None
    day = f(today.year)
    if day < today:
        day = f(today.year + 1)
    return key, day


# ---- the words -----------------------------------------------------------------------------------

def time_line(now: dt.datetime, place: str = "") -> str:
    clock = now.strftime("%-I:%M %p")
    return f"It's {clock} in {place}." if place else f"It's {clock}."


def date_line(now: dt.datetime) -> str:
    return f"It's {now.strftime('%A, %B')} {now.day}."


# WMO weather interpretation codes (Open-Meteo's weather_code), in words.
WMO = {
    0: "clear", 1: "mostly clear", 2: "partly cloudy", 3: "overcast", 45: "foggy", 48: "foggy",
    51: "light drizzle", 53: "drizzle", 55: "heavy drizzle", 56: "freezing drizzle", 57: "freezing drizzle",
    61: "light rain", 63: "rain", 65: "heavy rain", 66: "freezing rain", 67: "freezing rain",
    71: "light snow", 73: "snow", 75: "heavy snow", 77: "snow grains",
    80: "light showers", 81: "showers", 82: "heavy showers", 85: "snow showers", 86: "heavy snow showers",
    95: "thunderstorms", 96: "thunderstorms with hail", 99: "thunderstorms with hail",
}


def weather_line(place: str, data: Mapping[str, Any], day: int = 0) -> Optional[str]:
    """One spoken sentence from an Open-Meteo forecast; None when it lacks what the sentence needs."""
    try:
        daily = data["daily"]
        low, high = round(float(daily["temperature_2m_min"][day])), round(float(daily["temperature_2m_max"][day]))
        rain = daily.get("precipitation_probability_max", [None] * (day + 1))[day]
        day_sky = WMO.get(int(daily["weather_code"][day]), "") if "weather_code" in daily else ""
        if day == 0:
            cur = data["current"]
            temp, feels = round(float(cur["temperature_2m"])), round(float(cur["apparent_temperature"]))
            sky = WMO.get(int(cur["weather_code"]), "")
            wind = round(float(cur["wind_speed_10m"]))
    except (KeyError, IndexError, TypeError, ValueError):
        return None
    chance = f", {round(rain)}% chance of rain" if isinstance(rain, (int, float)) else ""
    if day == 1:
        return f"Tomorrow in {place}: {day_sky + ', ' if day_sky else ''}{low} to {high}°F{chance}."
    now = f"{temp}°F" + (f" and {sky}" if sky else "")
    if abs(feels - temp) >= 3:
        now += f", feels like {feels}°F"
    return f"In {place} it's {now}, wind {wind} mph. Today {low} to {high}°F{chance}."


def sun_line(place: str, data: Mapping[str, Any], which: str, day: int = 0) -> Optional[str]:
    try:
        stamp = data["daily"][which][day]
        at = dt.datetime.fromisoformat(stamp)
    except (KeyError, IndexError, TypeError, ValueError):
        return None
    when = "tomorrow" if day == 1 else "today"
    return f"{which.capitalize()} in {place} is at {at.strftime('%-I:%M %p')} {when}."


AQI_BANDS = [(50, "good"), (100, "moderate"), (150, "unhealthy for sensitive groups"), (200, "unhealthy"),
             (300, "very unhealthy"), (10_000, "hazardous")]


def air_line(place: str, data: Mapping[str, Any]) -> Optional[str]:
    try:
        aqi = round(float(data["current"]["us_aqi"]))
    except (KeyError, TypeError, ValueError):
        return None
    band = next(name for top, name in AQI_BANDS if aqi <= top)
    return f"The air in {place} is {band}: US AQI {aqi}."


def math_line(said: str, value: float) -> str:
    return f"{said[:1].upper()}{said[1:]} is {_num(value)}."


def convert_line(c: Mapping[str, Any]) -> str:
    return f"{_num(c['value'])} {c['src']} is {_num(c['result'])} {c['dst']}."


def countdown_line(name: str, day: dt.date, today: dt.date) -> str:
    n = (day - today).days
    pretty = name[:1].upper() + name[1:]
    if n == 0:
        return f"{pretty} is today!"
    return f"{n} day{'s' if n != 1 else ''} until {pretty}, on {day.strftime('%A, %B')} {day.day}."


def battery_line(pmset: str) -> Optional[str]:
    m = re.search(r"(\d+)%;\s*([A-Za-z ]+?);", pmset)
    if not m:
        return None
    state = m.group(2).strip().lower()
    plugged = "AC Power" in pmset
    words = {"charging": "charging", "charged": "fully charged", "discharging": "on battery",
             "finishing charge": "finishing its charge", "ac attached": "plugged in, not charging"}.get(state, state)
    if state == "discharging" and plugged:
        words = "plugged in"
    return f"The Mac is at {m.group(1)}%, {words}."


# ---- the answers ---------------------------------------------------------------------------------

def zone(environ: Optional[Mapping[str, str]] = None) -> Optional[dt.tzinfo]:
    env = os.environ if environ is None else environ
    name = (env.get("EVE_TIMEZONE") or "").strip()
    if name:
        try:
            return ZoneInfo(name)
        except (ZoneInfoNotFoundError, ValueError):
            log.warning("quick answers: EVE_TIMEZONE=%r is not a time zone; using the Mac's", name)
    return None


Getter = Callable[[str, dict[str, Any]], Awaitable[Any]]


async def _get_json(url: str, params: dict[str, Any], transport: Any = None) -> Any:
    import httpx

    hide_coordinates()                            # a test may have reset the logger's filters
    async with httpx.AsyncClient(timeout=TIMEOUT_SECS, transport=transport) as client:
        r = await client.get(url, params=params)
        r.raise_for_status()
        return r.json()


async def _pmset() -> str:
    proc = await asyncio.create_subprocess_exec("pmset", "-g", "batt", stdout=asyncio.subprocess.PIPE,
                                                stderr=asyncio.subprocess.DEVNULL)
    out, _ = await asyncio.wait_for(proc.communicate(), TIMEOUT_SECS)
    return out.decode("utf-8", "replace")


@dataclass
class Place:
    name: str
    lat: float
    lon: float
    tz: Optional[str] = None


def phone_here(environ: Optional[Mapping[str, str]] = None) -> Optional[Place]:
    """The phone's fresh fix as "here" (phone_location.fresh), rounded to PHONE_DECIMALS; None without one."""
    from . import phone_location

    fix = phone_location.fresh(environ, max_age_min=PHONE_FRESH_MIN)
    if fix is None:
        return None
    return Place(fix.place or PHONE_PLACE, round(fix.lat, PHONE_DECIMALS), round(fix.lon, PHONE_DECIMALS))


class QuickAnswers:
    """``match(text)`` then ``answer(ask)``: the words for a standard question, or None to let the model answer.
    ``phone``: where the phone is now, or None (phone_here, read on every question)."""

    def __init__(self, environ: Optional[Mapping[str, str]] = None, get: Getter = _get_json,
                 now: Callable[[Optional[dt.tzinfo]], dt.datetime] = dt.datetime.now,
                 pmset: Callable[[], Awaitable[str]] = _pmset,
                 phone: Optional[Callable[[], Optional[Place]]] = None) -> None:
        env = os.environ if environ is None else environ
        self.tz = zone(env)
        self.get, self.now, self.pmset = get, now, pmset
        self.phone = phone if phone is not None else (lambda: phone_here(env))
        self.home_name = " ".join((env.get("EVE_WEATHER_PLACE") or "").split())[:100]
        self.home: Optional[Place] = None
        try:
            lat, lon = env.get("EVE_WEATHER_LAT"), env.get("EVE_WEATHER_LON")
            if lat and lon:
                self.home = Place(self.home_name or "your area", float(lat), float(lon))
        except ValueError:
            log.warning("quick answers: EVE_WEATHER_LAT/LON are not numbers; ignored")
        self._places: dict[str, Optional[Place]] = {}

    @property
    def knows_here(self) -> bool:
        return self.home is not None or bool(self.home_name) or self._phone() is not None

    def _phone(self) -> Optional[Place]:
        try:
            return self.phone()
        except Exception as e:  # noqa: BLE001 — no phone fix: the configured place, as before
            log.info("quick answers: the phone's location unavailable (%s)", type(e).__name__)
            return None

    def match(self, text: str) -> Optional[Ask]:
        ask = match(text)
        if ask is None:
            return None
        if ask.kind in ("weather", "sun", "air") and not ask.place and not self.knows_here:
            return None                                   # "here" is not set: the model answers, as before
        return ask

    async def answer(self, ask: Ask) -> Optional[str]:
        try:
            return await self._answer(ask)
        except Exception as e:  # noqa: BLE001 — any failure: the model answers instead
            log.info("quick answers: %s unavailable (%s); the model answers", ask.kind, type(e).__name__)
            return None

    async def _answer(self, ask: Ask) -> Optional[str]:
        now = self.now(self.tz)
        if ask.kind == "time":
            if not ask.place:
                return time_line(now)
            place = await self._place(ask.place)
            if place is None or not place.tz:
                return None
            return time_line(dt.datetime.now(ZoneInfo(place.tz)), place.name)
        if ask.kind == "date":
            return date_line(now)
        if ask.kind == "math":
            return math_line(ask.args["said"], ask.args["value"])
        if ask.kind == "convert":
            return convert_line(ask.args)
        if ask.kind == "countdown":
            found = holiday(ask.args["what"], now.date())
            return countdown_line(found[0], found[1], now.date()) if found else None
        if ask.kind == "battery":
            return battery_line(await self.pmset())
        place = await self._place(ask.place) if ask.place else await self._here()
        if place is None:
            return None
        if ask.kind == "weather":
            data = await self.get(FORECAST_URL, {
                "latitude": place.lat, "longitude": place.lon,
                "current": "temperature_2m,apparent_temperature,weather_code,wind_speed_10m",
                "daily": "temperature_2m_max,temperature_2m_min,precipitation_probability_max,weather_code",
                "temperature_unit": "fahrenheit", "wind_speed_unit": "mph", "timezone": "auto", "forecast_days": 2,
            })
            return weather_line(place.name, data, ask.day) if isinstance(data, dict) else None
        if ask.kind == "sun":
            data = await self.get(FORECAST_URL, {"latitude": place.lat, "longitude": place.lon,
                                                 "daily": "sunrise,sunset", "timezone": "auto", "forecast_days": 2})
            return sun_line(place.name, data, ask.args["which"], ask.day) if isinstance(data, dict) else None
        if ask.kind == "air":
            data = await self.get(AIR_URL, {"latitude": place.lat, "longitude": place.lon,
                                            "current": "us_aqi", "timezone": "auto"})
            return air_line(place.name, data) if isinstance(data, dict) else None
        return None

    async def _here(self) -> Optional[Place]:
        phone = self._phone()
        if phone is not None:
            return phone                                  # the phone's fresh fix beats the configured home
        if self.home is None and self.home_name:
            self.home = await self._place(self.home_name)
        return self.home

    async def _place(self, name: str) -> Optional[Place]:
        key = name.lower()
        if key not in self._places:
            found = await self.get(GEOCODE_URL, {"name": name, "count": 1})
            hit = (found.get("results") or [None])[0] if isinstance(found, dict) else None
            self._places[key] = (Place(str(hit.get("name") or name), float(hit["latitude"]),
                                       float(hit["longitude"]), hit.get("timezone")) if hit else None)
        return self._places[key]
