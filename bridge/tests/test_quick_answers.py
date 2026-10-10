"""quick_answers.py: standard questions answered by code. What matches, what is left to the model, the words,
and the fall-back to the model when a source fails. Open-Meteo is faked; nothing here touches the network."""
from __future__ import annotations

import asyncio
import datetime as dt
from typing import Any

import pytest

from eve_bridge import quick_answers as qa
from eve_bridge.quick_answers import Ask, QuickAnswers, match

NOW = dt.datetime(2026, 9, 26, 16, 5, tzinfo=dt.timezone(dt.timedelta(hours=-7)))


@pytest.mark.parametrize("text,kind", [
    ("What time is it?", "time"), ("hey eve, what's the time", "time"), ("what time is it right now", "time"),
    ("Eve what time is it", "time"), ("can you tell me what time it is please", "time"),
    ("what's the date", "date"), ("what day is it today?", "date"), ("what's today's date", "date"),
    ("what's the weather", "weather"), ("how's the weather outside", "weather"), ("weather tomorrow", "weather"),
    ("is it going to rain today", "weather"), ("will it rain tomorrow?", "weather"), ("how cold is it", "weather"),
    ("do I need an umbrella", "weather"), ("what's the temperature outside", "weather"),
    ("when is sunset", "sun"), ("what time is sunrise tomorrow", "sun"), ("when does the sun set", "sun"),
    ("what's the air quality", "air"), ("aqi", "air"),
    ("what's my battery", "battery"), ("how much battery do I have left", "battery"),
    ("how many days until christmas", "countdown"), ("how long until Thanksgiving", "countdown"),
    ("what's 17 times 23", "math"), ("15 percent of 80", "math"), ("what is 2 to the power of 10", "math"),
    ("convert 5 km to miles", "convert"), ("72 fahrenheit in celsius", "convert"),
    ("how many ounces in a pound", "convert"), ("what's 3 cups in ml", "convert"),
])
def test_standard_questions_are_answered_by_code(text: str, kind: str) -> None:
    ask = match(text)
    assert ask is not None and ask.kind == kind, text


@pytest.mark.parametrize("text", [
    "what time is my meeting", "what time does the pharmacy close", "should I bike given the weather",
    "what's 17 times my rent", "remind me what the date of the party is", "what's 9/11", "open safari",
    "what's the weather been like this week in my photos", "how many days until my birthday",
    "time to go", "what is the time complexity of quicksort", "convert this pdf to word",
    "how many cups of coffee did I have", "what's 2026", "tell me about the weather in ancient rome and why",
])
def test_anything_more_goes_to_the_model(text: str) -> None:
    assert match(text) is None, text


def test_a_place_and_a_day_are_read() -> None:
    assert match("what's the weather in Tokyo tomorrow") == Ask("weather", place="tokyo", day=1)
    assert match("what time is it in New York") == Ask("time", place="new york")
    assert match("air quality in delhi") == Ask("air", place="delhi")


@pytest.mark.parametrize("text,value", [
    ("what's 17 times 23", 391), ("12 plus 30", 42), ("100 minus 1", 99), ("144 divided by 12", 12),
    ("15 percent of 80", 12), ("2 to the power of 10", 1024), ("what's 1,000 times 3", 3000),
    ("9 squared", 81), ("the square root of 81", 9), ("7 x 6", 42), ("(2 + 3) * 4", 20),
])
def test_the_sums_are_right(text: str, value: float) -> None:
    ask = match(text)
    assert ask is not None and ask.kind == "math" and ask.args["value"] == pytest.approx(value)


def test_no_sum_runs_away_or_runs_code() -> None:
    assert qa.arithmetic("2 ** 100000") is None                      # too big to compute
    assert qa.arithmetic("1 / 0") is None
    assert qa.arithmetic("__import__('os')") is None
    assert qa.arithmetic("what's 2 plus") is None


@pytest.mark.parametrize("text,result", [
    ("convert 5 km to miles", 3.10686), ("72 fahrenheit in celsius", 22.2222), ("100 c to f", 212),
    ("how many ounces in a pound", 16), ("what's 3 cups in ml", 709.765), ("10 kg to pounds", 22.0462),
    ("6 feet to cm", 182.88), ("60 mph to kph", 96.5606),
])
def test_the_conversions_are_right(text: str, result: float) -> None:
    ask = match(text)
    assert ask is not None and ask.kind == "convert" and ask.args["result"] == pytest.approx(result, rel=1e-4)


def test_a_conversion_across_dimensions_is_not_one() -> None:
    assert qa.conversion("5 km to pounds") is None


def test_countdowns_land_on_the_next_date() -> None:
    today = dt.date(2026, 9, 26)
    assert qa.holiday("christmas", today) == ("christmas", dt.date(2026, 12, 25))
    assert qa.holiday("thanksgiving", today) == ("thanksgiving", dt.date(2026, 11, 26))    # 4th Thursday
    assert qa.holiday("new year's", today) == ("new year's", dt.date(2027, 1, 1))
    assert qa.holiday("halloween", dt.date(2026, 11, 1))[1] == dt.date(2027, 10, 31)       # passed: next year
    assert qa.countdown_line("christmas", dt.date(2026, 12, 25), today) == \
        "90 days until Christmas, on Friday, December 25."
    assert qa.countdown_line("halloween", dt.date(2026, 10, 31), dt.date(2026, 10, 31)) == "Halloween is today!"


def test_the_words() -> None:
    assert qa.time_line(NOW) == "It's 4:05 PM."
    assert qa.date_line(NOW) == "It's Saturday, September 26."
    assert qa.math_line("17 * 23", 391) == "17 * 23 is 391."
    assert qa.battery_line("Now drawing from 'AC Power'\n -InternalBattery-0 (id=1)\t100%; charged; 0:00") == \
        "The Mac is at 100%, fully charged."
    assert qa.battery_line("Now drawing from 'Battery Power'\n -InternalBattery-0\t41%; discharging; 3:02") == \
        "The Mac is at 41%, on battery."
    assert qa.air_line("Springfield", {"current": {"us_aqi": 42}}) == "The air in Springfield is good: US AQI 42."


FORECAST = {
    "current": {"temperature_2m": 64.2, "apparent_temperature": 60.1, "weather_code": 2, "wind_speed_10m": 11.0},
    "daily": {"temperature_2m_min": [51.3, 49.0], "temperature_2m_max": [70.4, 66.6],
              "precipitation_probability_max": [10, 60], "weather_code": [2, 61],
              "sunrise": ["2026-09-26T07:03", "2026-09-27T07:04"], "sunset": ["2026-09-26T19:01", "2026-09-27T18:59"]},
}


def test_weather_words() -> None:
    assert qa.weather_line("Springfield", FORECAST) == \
        "In Springfield it's 64°F and partly cloudy, feels like 60°F, wind 11 mph. Today 51 to 70°F, 10% chance of rain."
    assert qa.weather_line("Springfield", FORECAST, day=1) == "Tomorrow in Springfield: light rain, 49 to 67°F, 60% chance of rain."
    assert qa.sun_line("Springfield", FORECAST, "sunset") == "Sunset in Springfield is at 7:01 PM today."
    assert qa.weather_line("Springfield", {"current": {}}) is None


class Api:
    def __init__(self, fail: bool = False) -> None:
        self.fail = fail
        self.calls: list[tuple[str, dict[str, Any]]] = []

    async def __call__(self, url: str, params: dict[str, Any]) -> Any:
        self.calls.append((url, params))
        if self.fail:
            raise OSError("offline")
        if url == qa.GEOCODE_URL:
            return {"results": [{"name": params["name"].title(), "latitude": 47.6, "longitude": -122.3,
                                 "timezone": "Asia/Tokyo"}]}
        return FORECAST


def quick(env: dict[str, str], api: Api) -> QuickAnswers:
    async def pmset() -> str:
        return "-InternalBattery-0\t77%; charging; 1:00"
    return QuickAnswers(env, get=api, now=lambda tz: NOW, pmset=pmset)


def test_here_is_looked_up_once_and_answers_come_from_code() -> None:
    api = Api()
    q = quick({"EVE_WEATHER_PLACE": "Springfield"}, api)

    async def go() -> list[Any]:
        a = await q.answer(q.match("what's the weather"))
        b = await q.answer(q.match("when is sunset"))
        return [a, b]

    a, b = asyncio.run(go())
    assert a.startswith("In Springfield it's 64°F") and b == "Sunset in Springfield is at 7:01 PM today."
    assert [u for u, _ in api.calls].count(qa.GEOCODE_URL) == 1


def test_without_a_home_place_weather_is_the_models() -> None:
    q = quick({}, Api())
    assert q.match("what's the weather") is None
    assert q.match("what's the weather in Paris") is not None           # a named place needs no home
    assert q.match("what time is it") is not None


def test_a_failed_source_hands_the_question_back_to_the_model() -> None:
    q = quick({"EVE_WEATHER_PLACE": "Springfield"}, Api(fail=True))
    assert asyncio.run(q.answer(q.match("what's the weather"))) is None
    assert asyncio.run(q.answer(q.match("what time is it"))) == "It's 4:05 PM."   # the clock never fails


def test_time_elsewhere_uses_the_places_zone() -> None:
    q = quick({}, Api())
    line = asyncio.run(q.answer(q.match("what time is it in tokyo")))
    assert line is not None and line.endswith("in Tokyo.")


def test_the_battery_is_the_macs() -> None:
    q = quick({}, Api())
    assert asyncio.run(q.answer(q.match("what's my battery"))) == "The Mac is at 77%, charging."
