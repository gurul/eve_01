"""phone_location.py: the phone's latest fix. Parsing the app's body, keeping only the newest fix (atomic, 0600), the
tool's answer (fresh, stale, none), the switch, the tool offered in every surface camera_status is (the chat brain,
the app chat, out loud, the taint gate, the intent eval's labels), quick answers' "here", and the live check tool.

Coordinates here are fictional: 0.0/0.0 and a city center (the repository is public)."""
from __future__ import annotations

import asyncio
import importlib.util
import json
import os
import re
import stat
import sys
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import phone_location as pl
from eve_bridge import quick_answers as qa
from eve_bridge import tool_gate

NOW = datetime(2026, 10, 12, 18, 0, tzinfo=timezone.utc)
CENTER = {"lat": 37.7749, "lon": -122.4194, "accuracy_m": 12.5, "at": "2026-10-12T17:55:00Z",
          "place": "San Francisco, CA", "speed_mps": 1.2}
TOOLS_DIR = Path(__file__).resolve().parents[1] / "tools"


def body(**kw: Any) -> dict[str, Any]:
    out = dict(CENTER)
    out.update(kw)
    return out


def fix(minutes_ago: float = 5.0, **kw: Any) -> pl.Fix:
    args: dict[str, Any] = dict(lat=0.0, lon=0.0, accuracy_m=10.0, place="Null Island",
                                at=datetime.now(timezone.utc) - timedelta(minutes=minutes_ago))
    args.update(kw)
    return pl.Fix(**args)


# ---- the body ------------------------------------------------------------------------------------------------

def test_a_good_body_is_a_fix() -> None:
    f = pl.parse(body(), now=NOW)
    assert (f.lat, f.lon, f.accuracy_m, f.place, f.speed_mps) == (37.7749, -122.4194, 12.5, "San Francisco, CA", 1.2)
    assert f.at == datetime(2026, 10, 12, 17, 55, tzinfo=timezone.utc)
    # an offset zone is the same instant; ints are numbers; null place and speed; keys beyond the six are ignored
    g = pl.parse(body(at="2026-10-12T10:55:00-07:00", lat=0, lon=0, place=None, speed_mps=None, extra=1), now=NOW)
    assert g.at == f.at and (g.lat, g.lon, g.place, g.speed_mps) == (0.0, 0.0, None, None)
    # iOS's "invalid speed" (-1) is no speed, not garbage
    assert pl.parse(body(speed_mps=-1), now=NOW).speed_mps is None
    # the place is one clean line
    assert pl.parse(body(place="  Market St\n\tSan Francisco  "), now=NOW).place == "Market St San Francisco"
    assert pl.parse(body(place="   "), now=NOW).place is None
    # the edges are in range
    assert pl.parse(body(lat=-90, lon=180, accuracy_m=0), now=NOW).lat == -90.0


@pytest.mark.parametrize("bad", [
    None, [], "x", {},
    body(lat=None), body(lat="37.7"), body(lat=True), body(lat=90.01), body(lat=-91), body(lat=float("nan")),
    body(lon=180.5), body(lon=float("inf")), body(lon=None),
    body(lat=10 ** 400), body(accuracy_m=-(10 ** 400)),        # integers JSON allows but no float holds
    body(accuracy_m=-1), body(accuracy_m=pl.MAX_ACCURACY_M + 1), body(accuracy_m=None), body(accuracy_m="5"),
    body(speed_mps=pl.MAX_SPEED_MPS + 1), body(speed_mps="fast"), body(speed_mps=False),
    body(at=None), body(at=1760290500), body(at="yesterday"), body(at="2026-10-12T17:55:00"),   # no zone
    body(at="2026-10-12T18:30:00Z"),                                                          # the future
    body(place=7), body(place=["a"]), body(place="x" * (pl.MAX_PLACE_CHARS + 1)),
])
def test_garbage_is_refused_with_a_reason(bad: Any) -> None:
    with pytest.raises(pl.BadFix) as e:
        pl.parse(bad, now=NOW)
    assert str(e.value) and "37.77" not in str(e.value)          # the reason never repeats a value


def test_a_little_clock_skew_is_allowed() -> None:
    assert pl.parse(body(at="2026-10-12T18:04:00Z"), now=NOW).at > NOW


# ---- keeping it --------------------------------------------------------------------------------------------

def test_only_the_newest_fix_is_kept_atomically_and_owner_only(tmp_path: Path) -> None:
    path = tmp_path / "state" / "phone-location.json"
    first = fix(10)
    assert pl.store(first, path) is True
    assert stat.S_IMODE(path.stat().st_mode) == 0o600
    assert stat.S_IMODE(path.parent.stat().st_mode) == 0o700           # a folder it made is owner-only too
    assert pl.load(path) == first
    # an older fix, or the same one again (a retry), is ignored and nothing is written
    before = path.read_bytes()
    assert pl.store(fix(20), path) is False and pl.store(first, path) is False
    assert path.read_bytes() == before
    # a newer one replaces it: one fix in the file, no history, no temp file left
    newer = fix(1, lat=1.5, place=None)
    assert pl.store(newer, path) is True and pl.load(path) == newer
    data = json.loads(path.read_text())
    assert set(data) == {"lat", "lon", "accuracy_m", "at", "place", "speed_mps"} and data["lat"] == 1.5
    assert sorted(p.name for p in path.parent.iterdir()) == ["phone-location.json"]
    assert stat.S_IMODE(path.stat().st_mode) == 0o600


def test_a_failed_write_leaves_the_old_fix_and_no_temp_file(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    path = tmp_path / "phone-location.json"
    old = fix(10)
    pl.store(old, path)

    def boom(*a: Any) -> None:
        raise OSError("disk full")

    monkeypatch.setattr(pl.os, "replace", boom)
    with pytest.raises(OSError):
        pl.store(fix(1), path)
    assert pl.load(path) == old and [p.name for p in tmp_path.iterdir()] == ["phone-location.json"]


def test_a_broken_file_is_no_fix_and_is_replaced(tmp_path: Path) -> None:
    path = tmp_path / "phone-location.json"
    path.write_text("{not json")
    assert pl.load(path) is None
    assert pl.store(fix(1), path) is True and pl.load(path) is not None
    path.write_text(json.dumps({"lat": 500}))
    assert pl.load(path) is None


def test_the_file_is_where_the_environment_says(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    assert pl.state_path({pl.FILE_ENV: str(tmp_path / "a.json")}) == tmp_path / "a.json"
    monkeypatch.delenv(pl.FILE_ENV)
    assert pl.state_path({}) == Path(pl.DEFAULT_FILE).expanduser()
    assert pl.DEFAULT_FILE.startswith("~/.config/eve-bridge/")


# ---- the tool ----------------------------------------------------------------------------------------------

def test_the_tool_is_strict_with_no_arguments_and_says_when_to_use_it() -> None:
    assert pl.TOOL_NAMES == ("phone_location",) and pl.TOOLS == [pl.TOOL]
    params = pl.TOOL["parameters"]
    assert pl.TOOL["strict"] is True and params == {"type": "object", "additionalProperties": False,
                                                    "required": [], "properties": {}}
    text = pl.TOOL["description"]
    for word in ("where they are", "near", "how long", "weather", "stale", "age_min", "accuracy"):
        assert word in text, word


def test_the_answer_fresh_stale_and_missing() -> None:
    assert asyncio.run(pl.handle("phone_location", {})) == {"error": pl.NO_FIX}
    taken = datetime.now(timezone.utc) - timedelta(minutes=5)
    pl.store(pl.Fix(lat=37.774912345, lon=-122.419412345, accuracy_m=12.4, at=taken, place="Null Island"))
    assert pl.reading(now=taken + timedelta(minutes=5, seconds=30)) == {
        "place": "Null Island", "lat": 37.77491, "lon": -122.41941, "accuracy_m": 12, "age_min": 5, "stale": False}
    out = asyncio.run(pl.handle("phone_location", {}))
    assert set(out) == {"place", "lat", "lon", "accuracy_m", "age_min", "stale"} and out["stale"] is False
    pl.store(fix(61))                       # older than the one kept: ignored
    assert asyncio.run(pl.handle("phone_location", {}))["stale"] is False
    path = pl.state_path()
    path.unlink()
    pl.store(fix(61))
    old = asyncio.run(pl.handle("phone_location", {}))
    assert old["stale"] is True and old["age_min"] >= 60
    # the boundary: exactly an hour is not stale, past it is
    at = datetime.now(timezone.utc)
    path.unlink()
    pl.store(pl.Fix(lat=0.0, lon=0.0, accuracy_m=5.0, at=at - timedelta(minutes=60)))
    assert pl.reading(now=at)["stale"] is False
    assert pl.reading(now=at + timedelta(seconds=1))["stale"] is True


def test_the_switch_turns_off_the_tool_and_its_answer(monkeypatch: pytest.MonkeyPatch) -> None:
    pl.store(fix(1))
    assert pl.enabled() and pl.offered() and pl.tools() == [pl.TOOL]
    for off in ("0", "false", "no", "off", " OFF "):
        monkeypatch.setenv(pl.SWITCH, off)
        assert not pl.enabled() and not pl.offered() and pl.tools() == []
        assert pl.fresh() is None
        out = asyncio.run(pl.handle("phone_location", {}))
        assert out == {"ok": False, "reason": pl.OFF_REASON} and "lat" not in out
    monkeypatch.setenv(pl.SWITCH, "1")
    assert pl.enabled() and pl.offered()


def test_the_tool_is_offered_only_once_the_phone_shared_a_fix() -> None:
    assert not pl.offered() and pl.tools() == []           # never shared: every prompt as before
    pl.store(fix(500))                                     # any age: the tool says it is stale
    assert pl.offered() and pl.tools() == [pl.TOOL]
    assert pl.fresh() is None                              # but it is not "here" for quick answers


def test_the_log_words_carry_age_and_accuracy_only() -> None:
    f = fix(2, lat=37.7749, lon=-122.4194, accuracy_m=8.4, place="San Francisco, CA")
    assert re.fullmatch(r"age 1[12]\d s, accuracy 8 m", pl.describe(f))


# ---- every surface camera_status is in ---------------------------------------------------------------------

def test_the_chat_brain_offers_and_answers_it_once_a_fix_is_kept(tmp_path: Path) -> None:
    from test_telegram import OWNER, FakeApi, FakeCreate, Rig, call, run_rig, say, update

    rig = Rig(FakeApi([update("where am i")]), FakeCreate(say("ok")))
    run_rig(rig)
    assert "phone_location" not in [t.get("name") for t in rig.create.requests[0]["tools"]]

    pl.store(fix(3, place="Null Island"))
    rig = Rig(FakeApi([update("where am i")]), FakeCreate(call("phone_location", {}), say("You're at Null Island.")))
    run_rig(rig)
    first = rig.create.requests[0]
    assert [t for t in first["tools"] if t.get("name") == "phone_location"] == [pl.TOOL]
    out = [json.loads(i["output"]) for i in rig.create.requests[1]["input"] if i.get("type") == "function_call_output"]
    assert out and out[-1]["place"] == "Null Island" and out[-1]["stale"] is False
    assert rig.api.sent[-1] == (OWNER, "You're at Null Island.")
    # a direct call reaches the module; off, the reason
    assert asyncio.run(rig.inlet._tool("phone_location", {}, OWNER))["place"] == "Null Island"


def test_the_chat_brain_off_neither_offers_nor_answers(monkeypatch: pytest.MonkeyPatch) -> None:
    from test_telegram import OWNER, FakeApi, FakeCreate, Rig, run_rig, say, update

    pl.store(fix(3))
    monkeypatch.setenv(pl.SWITCH, "0")
    rig = Rig(FakeApi([update("where am i")]), FakeCreate(say("ok")))
    run_rig(rig)
    assert "phone_location" not in [t.get("name") for t in rig.create.requests[0]["tools"]]
    assert asyncio.run(rig.inlet._tool("phone_location", {}, OWNER)) == {"ok": False, "reason": pl.OFF_REASON}


def test_its_result_is_not_written_to_the_transcript() -> None:
    from eve_bridge import telegram, voice_agent

    assert "phone_location" not in telegram.TRANSCRIBED_TOOLS
    assert "phone_location" not in voice_agent.ANSWER_TOOLS


def test_out_loud_it_is_offered_with_a_fix_never_in_a_lesson_and_answered() -> None:
    from test_voice_agent import FakeAgent, FakeConnection, _eventually, _session, _tool_call

    from eve_bridge import voice_agent
    from eve_bridge.voice_agent import VoiceConfig

    names = lambda cfg: [t.get("name") for t in cfg["delegation"]["responses"]["tools"]]  # noqa: E731
    assert "phone_location" not in names(voice_agent.session_config(VoiceConfig()))
    pl.store(fix(2, place="Null Island"))
    assert "phone_location" in names(voice_agent.session_config(VoiceConfig()))
    lesson = voice_agent.session_config(VoiceConfig(), think_aloud={"id": "l1", "topic": "fractions"})
    assert "phone_location" not in names(lesson)

    conn = FakeConnection([_tool_call("phone_location", "c1")])
    s, _, _ = _session(conn, [FakeAgent(None, None)])

    async def go() -> None:
        task = asyncio.create_task(s.run())
        await _eventually(lambda: bool(conn.tool_outputs()))
        conn.feed(_tool_call("end_conversation", "c9"), None)
        await task
    asyncio.run(go())
    out = conn.tool_outputs()[0]
    assert out["place"] == "Null Island" and out["stale"] is False
    assert s._taint == "the phone's location"            # a read: the place is the geocoder's words


def test_the_taint_gate_reads_it_as_a_read_that_never_asks() -> None:
    assert not tool_gate.acting("phone_location", {})
    assert not tool_gate.needs_confirmation("phone_location", {}, "a web search")
    assert tool_gate.taints("phone_location", {}) and tool_gate.source("phone_location") == "the phone's location"


def _intent_eval() -> Any:
    sys.path.insert(0, str(TOOLS_DIR))
    try:
        import intent_eval
    finally:
        sys.path.remove(str(TOOLS_DIR))
    return intent_eval


def test_the_intent_eval_has_its_label() -> None:
    ie = _intent_eval()
    assert "phone_location" in ie.LABELS and ie.TOOL_LABELS["phone_location"] == "phone_location"
    assert "phone_location" not in ie.acting_only_labels()


def test_the_intent_eval_lends_a_fictional_fresh_fix() -> None:
    _intent_eval()._phone_fix()
    f = pl.load(pl.state_path())
    assert f is not None and f.place == "San Francisco, CA" and pl.offered() and pl.fresh() is not None


# ---- quick answers' "here" ---------------------------------------------------------------------------------

class Api:
    def __init__(self) -> None:
        self.calls: list[tuple[str, dict[str, Any]]] = []

    async def __call__(self, url: str, params: dict[str, Any]) -> Any:
        self.calls.append((url, params))
        if url == qa.GEOCODE_URL:
            return {"results": [{"name": "Springfield", "latitude": 10.0, "longitude": 20.0}]}
        return {"current": {"temperature_2m": 60, "apparent_temperature": 60, "weather_code": 0,
                            "wind_speed_10m": 3},
                "daily": {"temperature_2m_min": [50, 50], "temperature_2m_max": [70, 70],
                          "precipitation_probability_max": [0, 0], "weather_code": [0, 0],
                          "sunrise": ["2026-10-12T07:00", "2026-10-13T07:01"],
                          "sunset": ["2026-10-12T18:30", "2026-10-13T18:29"]}}


def quick(env: dict[str, str], api: Api) -> qa.QuickAnswers:
    return qa.QuickAnswers(env, get=api, now=lambda tz: datetime(2026, 10, 12, 12, 0))


def test_a_fresh_phone_fix_is_here_rounded_and_named() -> None:
    pl.store(fix(5, lat=37.774912, lon=-122.419412, place="San Francisco, CA"))
    api = Api()
    q = quick({"EVE_WEATHER_LAT": "10", "EVE_WEATHER_LON": "20", "EVE_WEATHER_PLACE": "Springfield"}, api)
    line = asyncio.run(q.answer(q.match("what's the weather")))
    assert line is not None and line.startswith("In San Francisco, CA it's 60°F")
    assert (api.calls[-1][1]["latitude"], api.calls[-1][1]["longitude"]) == (37.77, -122.42)
    assert asyncio.run(q.answer(q.match("when is sunset"))) == "Sunset in San Francisco, CA is at 6:30 PM today."
    assert all(u != qa.GEOCODE_URL for u, _ in api.calls)


def test_with_no_place_name_it_is_where_you_are() -> None:
    pl.store(fix(5, place=None))
    q = quick({}, Api())
    assert q.match("what's the weather") is not None                 # no configured home: the phone is "here"
    assert asyncio.run(q.answer(q.match("what's the weather"))).startswith(f"In {qa.PHONE_PLACE} it's")


def test_a_stale_fix_or_the_switch_off_falls_back_to_the_configured_place(monkeypatch: pytest.MonkeyPatch) -> None:
    env = {"EVE_WEATHER_LAT": "10", "EVE_WEATHER_LON": "20", "EVE_WEATHER_PLACE": "Springfield"}
    pl.store(fix(61, place="San Francisco, CA"))
    api = Api()
    q = quick(env, api)
    assert asyncio.run(q.answer(q.match("what's the weather"))).startswith("In Springfield it's")
    assert (api.calls[-1][1]["latitude"], api.calls[-1][1]["longitude"]) == (10.0, 20.0)
    assert quick({}, Api()).match("what's the weather") is None       # stale and no home: the model answers
    pl.state_path().unlink()
    pl.store(fix(1, place="San Francisco, CA"))
    assert asyncio.run(q.answer(q.match("what's the weather"))).startswith("In San Francisco, CA it's")
    off = quick({**env, pl.SWITCH: "0"}, Api())               # the switch, from the environment it was given
    assert asyncio.run(off.answer(off.match("what's the weather"))).startswith("In Springfield it's")
    monkeypatch.setenv(pl.SWITCH, "0")                         # and the daemon's (os.environ) for the default
    for key in ("EVE_WEATHER_LAT", "EVE_WEATHER_LON", "EVE_WEATHER_PLACE"):
        monkeypatch.delenv(key, raising=False)
    q = qa.QuickAnswers(get=Api(), now=lambda tz: datetime(2026, 10, 12, 12, 0))
    assert q.match("what's the weather") is None


def test_a_named_place_still_beats_the_phone() -> None:
    pl.store(fix(1, place="San Francisco, CA"))
    q = quick({}, Api())
    assert asyncio.run(q.answer(q.match("what's the weather in springfield"))).startswith("In Springfield it's")


def test_a_broken_phone_reader_never_breaks_an_answer() -> None:
    def boom() -> Any:
        raise RuntimeError("no")

    q = qa.QuickAnswers({"EVE_WEATHER_LAT": "10", "EVE_WEATHER_LON": "20"}, get=Api(), phone=boom,
                        now=lambda tz: datetime(2026, 10, 12, 12, 0))
    assert asyncio.run(q.answer(q.match("what's the weather"))).startswith("In your area it's")


def test_the_phones_coordinates_never_reach_the_log(caplog: pytest.LogCaptureFixture) -> None:
    """A real httpx client (a mock transport, nothing leaves the Mac) at the daemon's INFO level: httpx logs the
    request line, and the Open-Meteo coordinates in it read <hidden>. Positive control: with the filter taken off,
    the same answer does put the coordinates in the log, so this test can see a leak."""
    import logging

    import httpx

    pl.store(fix(5, lat=37.774912, lon=-122.419412, place="San Francisco, CA"))
    seen: list[str] = []

    def reply(request: httpx.Request) -> httpx.Response:
        seen.append(str(request.url))
        return httpx.Response(200, json={"current": {"temperature_2m": 60, "apparent_temperature": 60,
                                                     "weather_code": 0, "wind_speed_10m": 3},
                                         "daily": {"temperature_2m_min": [50, 50], "temperature_2m_max": [70, 70],
                                                   "precipitation_probability_max": [0, 0],
                                                   "weather_code": [0, 0]}})

    async def get(url: str, params: dict[str, Any]) -> Any:
        return await qa._get_json(url, params, transport=httpx.MockTransport(reply))

    def ask() -> str:
        q = qa.QuickAnswers({}, get=get, now=lambda tz: datetime(2026, 10, 12, 12, 0))
        return asyncio.run(q.answer(q.match("what's the weather")))

    caplog.set_level(logging.INFO, logger="httpx")
    assert ask().startswith("In San Francisco, CA it's")
    assert "latitude=37.77" in seen[-1]                                     # Open-Meteo was asked with them
    lines = [r.getMessage() for r in caplog.records if r.name == "httpx"]
    assert any("api.open-meteo.com" in m and "latitude=" + qa.HIDDEN in m and "longitude=" + qa.HIDDEN in m
               for m in lines)
    assert not any("37.77" in m or "-122.4" in m for m in lines)

    logger = logging.getLogger("httpx")
    held = [f for f in logger.filters if isinstance(f, qa.CoordinateFilter)]
    caplog.clear()
    try:
        for f in held:
            logger.removeFilter(f)
        async def bare(url: str, params: dict[str, Any]) -> Any:     # _get_json without re-installing the filter
            async with httpx.AsyncClient(transport=httpx.MockTransport(reply)) as client:
                return (await client.get(url, params=params)).json()

        q = qa.QuickAnswers({}, get=bare, now=lambda tz: datetime(2026, 10, 12, 12, 0))
        asyncio.run(q.answer(q.match("what's the weather")))
        assert any("latitude=37.77" in r.getMessage() for r in caplog.records if r.name == "httpx")
    finally:
        qa.hide_coordinates()


def test_the_filter_only_touches_open_meteo_urls() -> None:
    import logging

    f = qa.CoordinateFilter()
    other = logging.LogRecord("httpx", logging.INFO, "", 0, "HTTP Request: GET %s", ("https://example.com/?latitude=1",),
                              None)
    assert f.filter(other) and other.getMessage().endswith("latitude=1")
    meteo = logging.LogRecord("httpx", logging.INFO, "", 0, "HTTP Request: GET %s \"%s\"",
                              ("https://api.open-meteo.com/v1/forecast?latitude=0.0&longitude=-0.5&x=1", "200"), None)
    assert f.filter(meteo)
    assert meteo.getMessage() == ("HTTP Request: GET https://api.open-meteo.com/v1/forecast?latitude=<hidden>"
                                  "&longitude=<hidden>&x=1 \"200\"")


# ---- the live check ----------------------------------------------------------------------------------------

def _check() -> Any:
    spec = importlib.util.spec_from_file_location("phone_location_check", TOOLS_DIR / "phone_location_check.py")
    assert spec is not None and spec.loader is not None
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def test_the_check_prints_only_age_and_accuracy(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    check = _check()
    state = tmp_path / "fix.json"
    env_file = tmp_path / "env"
    env_file.write_text(f"OPENROUTER_API_KEY=placeholder-value\nEVE_PHONE_LOCATION_FILE={state}\n")
    os.chmod(env_file, 0o600)
    assert check.main(["--env-file", str(env_file)]) == 1
    assert "no location" in capsys.readouterr().out
    pl.store(fix(4, lat=37.7749, lon=-122.4194, accuracy_m=9.0, place="San Francisco, CA"), state)
    assert check.main(["--env-file", str(env_file)]) == 0
    out = capsys.readouterr().out
    assert out.splitlines()[-1] == "LOCATION_LIVE_OK" and "accuracy 9 m" in out and "age 4." in out
    for secret in ("37.7", "122.4", "San Francisco", "placeholder-value", str(tmp_path)):
        assert secret not in out
    state.unlink()
    pl.store(fix(45), state)
    assert check.main(["--env-file", str(env_file)]) == 1
    out = capsys.readouterr().out
    assert "LOCATION_LIVE_OK" not in out and "age 45." in out


def test_the_check_reads_the_daemons_switch_and_default_path(tmp_path: Path, capsys: pytest.CaptureFixture[str]) -> None:
    check = _check()
    env_file = tmp_path / "env"
    env_file.write_text("EVE_PHONE_LOCATION=0\n")
    state = tmp_path / "fix.json"
    pl.store(fix(1), state)
    assert check.main(["--env-file", str(env_file), "--state", str(state)]) == 1
    assert "off" in capsys.readouterr().out
    assert check.daemon_env(tmp_path / "missing") == {}
    assert check.KEYS == (pl.SWITCH, pl.FILE_ENV)
