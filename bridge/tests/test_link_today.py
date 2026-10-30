"""link_today.py: the home-screen widget's one read, ``GET /api/link/today`` (docs/widget.md).

The real Mini App server on a loopback port and test_link_chat's raw HTTP client, a real Chief with three cards
waiting on their Go, real watch.Watch rows, and fakes for the calendar (apps_mcp.AppsBridge.execute's shape) and the
Music and TV remote cards (``panel(body)``)."""
from __future__ import annotations

import asyncio
import json
import threading
import time
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Optional

from test_link_chat import FakeInlet, FakePanel, chief_at, http, serve, store_at

from eve_bridge import apps_mcp, link_chat, watch
from eve_bridge.link_today import Today, events_part, music_part, signin_part, tv_part

NOW = datetime(2026, 10, 8, 14, 0).astimezone()        # 2 pm local


def at(hour: int, minute: int = 0, days: int = 0) -> str:
    return (datetime(2026, 10, 8, hour, minute) + timedelta(days=days)).astimezone().isoformat()


def ev(summary: str, start: str, end: str, **kw: Any) -> dict[str, Any]:
    return {"id": summary, "summary": summary, "start": {"dateTime": start}, "end": {"dateTime": end}, **kw}


EVENTS = [
    ev("Breakfast", at(8), at(9)),                                           # over
    ev("Design review", at(13, 30), at(14, 30), location="Room 4"),           # on now
    ev("Dentist", at(16), at(17)),
    ev("Declined sync", at(17), at(18), attendees=[{"self": True, "responseStatus": "declined"}]),
    ev("Cancelled call", at(18), at(19), status="cancelled"),
    ev("Tomorrow's standup", at(9, days=1), at(10, days=1)),                  # not today
    {"id": "trip", "summary": "Trip", "start": {"date": "2026-10-08"}, "end": {"date": "2026-10-09"}},
]


class FakeApps:
    """apps_mcp.AppsBridge as link_today uses it: ``apps`` and a blocking ``execute`` (called on a thread)."""

    def __init__(self, events: Optional[list[dict[str, Any]]] = None, *, fail: bool = False, delay: float = 0.0,
                 apps: tuple[str, ...] = ("calendar",)) -> None:
        self.apps = list(apps)
        self.events = events if events is not None else EVENTS
        self.fail, self.delay = fail, delay
        self.calls: list[tuple[str, dict[str, Any]]] = []
        self.lock = threading.Lock()

    def execute(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        with self.lock:
            self.calls.append((name, args))
        if self.delay:
            time.sleep(self.delay)
        if self.fail:
            return {"ok": False, "reason": "GoogleError: 401"}
        return {"ok": True, "result": {"timeZone": "America/Los_Angeles", "events": self.events}}


class SlowPanel(FakePanel):
    async def panel(self, body: dict[str, Any]) -> dict[str, Any]:
        await asyncio.sleep(5)
        return self.answer


class BrokenPanel(FakePanel):
    async def panel(self, body: dict[str, Any]) -> dict[str, Any]:
        raise OSError("adb went away")


class Mono:
    def __init__(self) -> None:
        self.t = 1000.0

    def __call__(self) -> float:
        return self.t


def watches() -> Any:
    class W:
        pass

    w = W()
    w.watches = [
        watch.Watch(id="w1", kind="price", target="https://shop.example/chair", label="Desk chair", condition="below",
                    value=300, currency="USD", checks=3, last_value=320.0),
        watch.Watch(id="w2", kind="price", target="https://shop.example/lamp", label="Lamp", condition="below",
                    value=40, paused=True),
        watch.Watch(id="w3", kind="quote", target="AAPL", label="AAPL", condition="below", value=200,
                    errors=watch.TELL_AFTER_ERRORS, checks=9),
        watch.Watch(id="w4", kind="price", target="https://shop.example/desk", label="Desk", condition="change"),
    ]
    return w


def today_door(**kw: Any) -> Today:
    kw.setdefault("app_expiry", lambda: None)       # never the Mac's real last build
    return Today(now=lambda: NOW, **kw)


async def today(port: int, token: str) -> dict[str, Any]:
    status, headers, body = await http(port, "GET", link_chat.TODAY_PATH, token=token)
    assert status == 200, body
    assert headers.get("cache-control") == "no-store"
    return json.loads(body)


# ---- who may call ----------------------------------------------------------------------------------------

def test_today_needs_the_link_token_no_origin_and_a_get(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, FakeInlet(store_at(tmp_path)), today=today_door())
        path = link_chat.TODAY_PATH
        assert (await http(port, "GET", path))[0] == 401                                    # no token
        assert (await http(port, "GET", path, token=token[:-1]))[0] == 401                   # nearly right
        assert (await http(port, "GET", path, token=token, origin="https://evil.example"))[0] == 403
        assert (await http(port, "GET", path, token=token, origin="null"))[0] == 403
        assert (await http(port, "POST", path, {}, token=token))[0] == 405
        assert (await http(port, "GET", path + "/x", token=token))[0] == 404
        assert (await http(port, "GET", path, token=token))[0] == 200                       # positive control
        await server.close()

    asyncio.run(go())


# ---- what it gathers -------------------------------------------------------------------------------------

def test_today_gathers_jobs_watches_calendar_and_what_plays(tmp_path: Path) -> None:
    c, _, _ = chief_at(tmp_path)
    apps = FakeApps()

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet.chief = c
        inlet._watcher = watches()
        inlet._direct = apps
        server, port, token = await serve(tmp_path, inlet, today=today_door())
        server.spotify = FakePanel({"ok": True, "playing": True, "track": "A song", "artists": ["Someone", "Else"]})
        server.firetv = FakePanel({"ok": True, "power": "on", "app": "YouTube", "volume": 40, "line": "TV on"})
        got = await today(port, token)
        assert got["sources"] == {"jobs": "ok", "watches": "ok", "calendar": "ok", "work_calendar": "off", "music": "ok",
                                  "tv": "ok"}
        assert got["at"] == NOW.timestamp()
        # the chief: three cards waiting on their Go, all needing the owner, none merely going
        assert got["needs_you"] == 3 and [n["id"] for n in got["needs"]] == ["c1", "c2", "c3"]
        assert all(n["kind"] == "job" and n["line"] == "Waiting on your Go" and n["title"] for n in got["needs"])
        # the watches that are on, after the chief's going cards (none here); the paused lamp is left out
        assert [(g["id"], g["kind"]) for g in got["going"]] == [("w1", "watch"), ("w3", "watch"), ("w4", "watch")]
        assert got["going"][0]["title"] == "Desk chair" and got["going"][0]["line"] == watch.now_line(
            inlet._watcher.watches[0])
        assert got["going"][1]["line"] == "can't read it" and got["going"][2]["line"] == "no reading yet"
        # the calendar: the all-day trip first, then what has not ended, soonest first; nothing tomorrow, declined
        # or cancelled
        assert [e["title"] for e in got["today"]] == ["Trip", "Design review", "Dentist"]
        trip, review, dentist = got["today"]
        assert trip["all_day"] is True and review["all_day"] is False and review["place"] == "Room 4"
        assert review["start"] == datetime.fromisoformat(at(13, 30)).timestamp()
        assert dentist["end"] == datetime.fromisoformat(at(17)).timestamp()
        name, args = apps.calls[0]
        assert name == "calendar__list_events"
        assert datetime.fromisoformat(args["startTime"]) == NOW
        assert datetime.fromisoformat(args["endTime"]) == datetime(2026, 10, 9).astimezone()
        # what plays: Spotify's track and the TV's app, read with the cards' own status action
        assert got["now"] == [{"kind": "music", "title": "A song", "line": "Someone, Else"},
                              {"kind": "tv", "title": "TV", "line": "YouTube"}]
        assert server.spotify.bodies == [{"action": "status"}] and server.firetv.bodies == [{"action": "status"}]
        await server.close()

    asyncio.run(go())


class FakeComposio:
    """composio_tools.ComposioBridge as updates_composio.Reader uses it: the connected accounts and a read."""

    can_read = True

    def __init__(self, accounts: list[dict[str, Any]], events: list[dict[str, Any]]) -> None:
        self.accounts, self.events = accounts, events
        self.reads: list[tuple[str, dict[str, Any], str]] = []

    def connected_accounts(self) -> list[dict[str, Any]]:
        return self.accounts

    def read_tool(self, slug: str, args: dict[str, Any], account: str) -> dict[str, Any]:
        self.reads.append((slug, args, account))
        return {"ok": True, "data": {"items": self.events}}


WORK = {"toolkit": "googlecalendar", "id": "ca_work", "alias": "work", "status": "ACTIVE"}


def test_today_adds_the_work_calendar_from_composio(tmp_path: Path) -> None:
    composio = FakeComposio([WORK, {"toolkit": "googlecalendar", "id": "ca_me", "alias": "personal",
                                    "status": "ACTIVE"}],
                            [ev("Team sync", at(15), at(15, 30)), ev("Dentist", at(16), at(17)),      # also personal
                             ev("Tomorrow's offsite", at(9, days=1), at(17, days=1))])

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet._direct = FakeApps()
        inlet._apps = composio
        server, port, token = await serve(tmp_path, inlet, today=today_door())
        got = await today(port, token)
        assert got["sources"]["calendar"] == "ok" and got["sources"]["work_calendar"] == "ok"
        # merged in time order, the shared dentist once, tomorrow left out
        assert [e["title"] for e in got["today"]] == ["Trip", "Design review", "Team sync", "Dentist"]
        # only the work account is read (the personal one comes through the direct Google app), today only
        assert [(slug, acct) for slug, _, acct in composio.reads] == [("GOOGLECALENDAR_EVENTS_LIST", "ca_work")]
        args = composio.reads[0][1]
        assert datetime.fromisoformat(args["timeMin"]) == NOW
        assert datetime.fromisoformat(args["timeMax"]) == datetime(2026, 10, 9).astimezone()
        got = await today(port, token)                               # kept: no second read
        assert len(composio.reads) == 1 and len(got["today"]) == 4
        await server.close()

    asyncio.run(go())


def test_no_work_calendar_connected_is_off(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet._apps = FakeComposio([{"toolkit": "slack", "id": "ca_s", "alias": "work", "status": "ACTIVE"}], [])
        server, port, token = await serve(tmp_path, inlet, today=today_door())
        got = await today(port, token)
        assert got["sources"]["work_calendar"] == "off" and inlet._apps.reads == []
        await server.close()

    asyncio.run(go())


def test_open_cards_that_do_not_need_the_owner_are_going(tmp_path: Path) -> None:
    c, _, _ = chief_at(tmp_path)

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet.chief = c
        assert c.approve("c2", via="id")["ok"]                     # approved, not yet run: going, not needing you
        c.drop("c3")                                               # finished: on neither list
        server, port, token = await serve(tmp_path, inlet, today=today_door())
        got = await today(port, token)
        assert got["needs_you"] == 1 and [n["id"] for n in got["needs"]] == ["c1"]
        assert [g["id"] for g in got["going"]] == ["c2"] and got["going"][0]["kind"] == "job"
        assert got["sources"]["watches"] == "off" and got["sources"]["calendar"] == "off"
        await server.close()

    asyncio.run(go())


def test_nothing_on_is_an_empty_answer_not_an_error(tmp_path: Path) -> None:
    async def go() -> None:
        server, port, token = await serve(tmp_path, None, today=today_door())    # no chat, no chief, no cards
        got = await today(port, token)
        assert got == {"at": NOW.timestamp(), "needs_you": 0, "needs": [], "going": [], "today": [], "now": [],
                       "sources": {k: "off" for k in ("jobs", "watches", "calendar", "work_calendar", "music", "tv")}}
        await server.close()

    asyncio.run(go())


def test_a_calendar_that_is_not_signed_in_is_off(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet._direct = FakeApps(apps=("gmail", "drive"))
        server, port, token = await serve(tmp_path, inlet, today=today_door())
        got = await today(port, token)
        assert got["sources"]["calendar"] == "off" and inlet._direct.calls == []
        await server.close()

    asyncio.run(go())


# ---- partial results -------------------------------------------------------------------------------------

def test_a_slow_or_broken_source_is_left_out_and_the_rest_still_come(tmp_path: Path) -> None:
    c, _, _ = chief_at(tmp_path)

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet.chief = c
        inlet._direct = FakeApps(fail=True)
        door = today_door(source_secs=0.3, calendar_secs=0.3)
        server, port, token = await serve(tmp_path, inlet, today=door)
        server.spotify = SlowPanel({"ok": True, "playing": True, "track": "x"})
        server.firetv = BrokenPanel({})
        started = time.monotonic()
        got = await today(port, token)
        assert time.monotonic() - started < 3.0                    # the slow card did not hold the answer
        assert got["sources"] == {"jobs": "ok", "watches": "off", "calendar": "failed", "work_calendar": "off",
                                  "music": "failed", "tv": "failed"}
        assert got["needs_you"] == 3 and got["today"] == [] and got["now"] == []
        inlet.chief = None
        inlet._watcher = type("Broken", (), {"watches": property(lambda self: 1 / 0)})()
        got = await today(port, token)
        assert got["sources"]["jobs"] == "off" and got["sources"]["watches"] == "failed" and got["going"] == []
        await server.close()

    asyncio.run(go())


def test_a_slow_calendar_is_left_out_then_kept_for_the_next_refresh(tmp_path: Path) -> None:
    apps = FakeApps(delay=0.6)
    mono = Mono()

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet._direct = apps
        server, port, token = await serve(tmp_path, inlet, today=today_door(calendar_secs=0.2, mono=mono))
        got = await today(port, token)
        assert got["sources"]["calendar"] == "failed" and got["today"] == []
        await asyncio.sleep(0.8)                                   # the read finished after the answer went
        got = await today(port, token)
        assert got["sources"]["calendar"] == "ok" and len(got["today"]) == 3
        assert len(apps.calls) == 1                                # kept, not read again
        mono.t += 301                                              # past CALENDAR_KEEP_SECS: read again
        apps.delay = 0
        await today(port, token)
        assert len(apps.calls) == 2
        await server.close()

    asyncio.run(go())


# ---- the pieces ------------------------------------------------------------------------------------------

def test_events_part_keeps_today_only_and_reads_both_shapes() -> None:
    raw = {"ok": True, "result": {"events": EVENTS + [{"summary": "no start"}, {"id": "bad", "summary": "Bad",
                                                                                  "start": {"dateTime": "soon"}}]}}
    assert [e["title"] for e in events_part(raw, NOW)] == ["Trip", "Design review", "Dentist"]
    late = datetime(2026, 10, 8, 23, 30).astimezone()
    assert [e["title"] for e in events_part(raw, late)] == ["Trip"]
    untitled = events_part({"result": {"events": [{**ev("x", at(15), at(16)), "summary": ""}]}}, NOW)
    assert [e["title"] for e in untitled] == ["Busy"]                       # a private event still holds the time
    assert events_part(None, NOW) == [] and events_part("clipped text", NOW) == []


def test_music_and_tv_rows_only_while_something_plays() -> None:
    assert music_part({"ok": True, "playing": False, "track": "x"}) == []
    assert music_part({"ok": False, "reason": "Couldn't reach Spotify."}) == []
    assert music_part({"ok": True, "playing": True, "track": None}) == []
    assert music_part({"ok": True, "playing": True, "track": "x" * 200, "artists": []})[0]["title"].endswith("…")
    assert tv_part({"ok": True, "power": "standby"}) == []
    assert tv_part({"ok": False, "reachable": False}) == []
    assert tv_part({"ok": True, "power": "screensaver"}) == [{"kind": "tv", "title": "TV", "line": "Screensaver"}]
    assert tv_part({"ok": True, "power": "on", "app": None}) == [{"kind": "tv", "title": "TV", "line": "On"}]


# ---- sign-ins (owner, 2026-10-26: "send a notification on the widget when u need to login") ---------------------

def token_at(tmp_path: Path, signed: datetime) -> Path:
    path = tmp_path / "google-token.json"
    path.write_text(json.dumps({"refresh_token": "r", "signed_in_at": signed.timestamp()}))
    return path


def test_a_refused_google_refresh_asks_to_sign_in_until_a_newer_sign_in() -> None:
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        token = token_at(Path(d), NOW - timedelta(days=8))
        rows = signin_part(token, NOW, refused=NOW.timestamp() - 60, app_expiry=lambda: None)
        assert [(r["id"], r["kind"], r["title"]) for r in rows] == [("signin-google", "signin", "Sign in to Google")]
        assert "apps login google" in rows[0]["line"] and len(rows[0]["line"]) <= 60
        # signed in again after the refusal: nothing to ask, and the new sign-in has 7 days left
        token = token_at(Path(d), NOW)
        assert signin_part(token, NOW, refused=NOW.timestamp() - 60, app_expiry=lambda: None) == []


def test_a_sign_in_or_install_that_lapses_soon_shows_before_it_does() -> None:
    import tempfile
    with tempfile.TemporaryDirectory() as d:
        token = token_at(Path(d), NOW - timedelta(days=6, hours=20))          # lapses in 4 h
        app = NOW + timedelta(days=1, hours=1)                                 # tomorrow 3 pm
        rows = signin_part(token, NOW, app_expiry=lambda: app)
        assert [r["title"] for r in rows] == ["Google sign-in ends in 4 h", "Eve Link stops opening tomorrow 3:00 pm"]
        assert rows[1]["id"] == "signin-app" and "build.sh --install" in rows[1]["line"]
        # three days out: quiet; no Google set up and no build: quiet
        token = token_at(Path(d), NOW - timedelta(days=4))
        assert signin_part(token, NOW, app_expiry=lambda: NOW + timedelta(days=3)) == []
        assert signin_part(None, NOW, refused=NOW.timestamp(), app_expiry=lambda: None) == []


def test_sign_ins_come_first_in_needs_you_on_the_widget(tmp_path: Path) -> None:
    c, _, _ = chief_at(tmp_path)

    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path))
        inlet.chief = c
        app = NOW + timedelta(hours=2)
        server, port, token = await serve(tmp_path, inlet, today=today_door(app_expiry=lambda: app))
        got = await today(port, token)
        assert got["needs"][0] == {"id": "signin-app", "kind": "signin", "title": "Eve Link stops opening in 2 h",
                                   "line": "build.sh --install on the Mac"}
        assert got["needs_you"] == 4 and [n["id"] for n in got["needs"][1:]] == ["c1", "c2", "c3"]
        await server.close()

    asyncio.run(go())


def test_a_google_refusal_is_recorded_and_a_good_refresh_clears_it(tmp_path: Path) -> None:
    class Http:
        def __init__(self, body: dict[str, Any]) -> None:
            self.body = body

        def post(self, url: str, data: Any) -> Any:
            body = self.body

            class R:
                def json(self) -> dict[str, Any]:
                    return body
            return R()

    cfg = apps_mcp.AppsConfig(enabled=True, config_dir=tmp_path)
    (tmp_path / "google-oauth-client.json").write_text(json.dumps({"installed": {"client_id": "id"}}))
    cfg.google_token_path.write_text(json.dumps({"refresh_token": "r"}))
    apps_mcp.refusals.clear()
    assert apps_mcp.TokenSource("google", cfg, http=Http({"error": "invalid_grant"}))() is None
    assert "google" in apps_mcp.refusals
    assert apps_mcp.TokenSource("google", cfg, http=Http({"access_token": "a", "expires_in": 3600}))() == "a"
    assert "google" not in apps_mcp.refusals
