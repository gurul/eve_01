"""canvas.py: the owner's Canvas, read only, over a fake HTTP transport (no network anywhere).

Pins: the settings (off when unset, https only), each tool's parsing, Link-header pagination and its cap, due
times in the owner's zone, the dead-token line, a course that refuses without sinking the answer, and that no
request but a GET can leave (with a positive control: a POST is refused before the transport sees it).
"""

from __future__ import annotations

import asyncio
import json
import urllib.parse
from datetime import datetime, timezone
from typing import Any, Callable
from zoneinfo import ZoneInfo

import httpx
import pytest
from test_telegram import OWNER, FakeApi, FakeCreate, Rig, call, say

from eve_bridge import canvas, rundown, self_context, telegram

BASE = "https://canvas.example.edu"
TOKEN = "test-canvas-token"
ENV = {"CANVAS_BASE_URL": BASE, "CANVAS_API_TOKEN": TOKEN}
NOW = datetime(2026, 9, 30, 17, 0, tzinfo=timezone.utc)          # Wed Sep 30, 10:00 AM in Seattle
PT = ZoneInfo("America/Los_Angeles")

COURSES = [
    {"id": 101, "name": "Systems Programming", "course_code": "CSE 333 A", "workflow_state": "available",
     "term": {"name": "Autumn 2026", "end_at": "2026-12-20T08:00:00Z"}},
    {"id": 102, "name": "Natural Language Processing", "course_code": "CSE 447 A", "workflow_state": "available",
     "term": {"name": "Autumn 2026", "end_at": None}},
    {"id": 90, "name": "Old course", "course_code": "CSE 143", "term": {"name": "Spring 2025",
                                                                          "end_at": "2025-06-15T07:00:00Z"}},
    {"id": 91, "access_restricted_by_date": True},
]


class Fake:
    """Routes by path; records every request the transport saw."""

    def __init__(self, routes: dict[str, Any]) -> None:
        self.routes = routes
        self.seen: list[httpx.Request] = []

    def __call__(self, request: httpx.Request) -> httpx.Response:
        self.seen.append(request)
        route = self.routes.get(request.url.path)
        if route is None:
            return httpx.Response(404, json={"errors": [{"message": "The specified resource does not exist."}]})
        if callable(route):
            return route(request)
        if isinstance(route, httpx.Response):
            return route
        return httpx.Response(200, json=route)

    def hub(self, tz: Any = PT) -> canvas.Canvas:
        return canvas.Canvas(canvas.CanvasApi(canvas.configured(ENV), httpx.MockTransport(self)), tz=tz,
                             now=lambda: NOW)


def routes(**extra: Any) -> dict[str, Any]:
    return {"/api/v1/courses": COURSES, **extra}


# ---- settings ------------------------------------------------------------------------------------------------

def test_off_when_unset_or_half_set_or_not_https() -> None:
    assert canvas.configured({}).enabled is False
    assert canvas.make_canvas({}) is None
    assert canvas.configured({"CANVAS_BASE_URL": BASE}).enabled is False
    assert canvas.configured({"CANVAS_API_TOKEN": TOKEN}).enabled is False
    assert canvas.configured({**ENV, "CANVAS_BASE_URL": "http://canvas.example.edu"}).enabled is False
    assert canvas.configured({**ENV, "EVE_CANVAS": "0"}).enabled is False
    cfg = canvas.configured({**ENV, "CANVAS_BASE_URL": "https://Canvas.Example.edu/api/v1/"})
    assert cfg.enabled and cfg.base_url == BASE
    assert TOKEN not in repr(cfg)
    assert isinstance(canvas.make_canvas(ENV, httpx.MockTransport(Fake({}))), canvas.Canvas)


# ---- courses, pagination -------------------------------------------------------------------------------------

def test_courses_are_the_current_ones_and_the_token_rides_the_header() -> None:
    fake = Fake(routes())
    out = fake.hub().call("canvas_courses", {})
    assert out["ok"] and [c["code"] for c in out["courses"]] == ["CSE 333 A", "CSE 447 A"]
    assert out["courses"][0] == {"id": 101, "name": "Systems Programming", "code": "CSE 333 A", "term": "Autumn 2026"}
    req = fake.seen[0]
    assert req.headers["authorization"] == f"Bearer {TOKEN}"
    q = urllib.parse.parse_qs(req.url.query.decode())
    assert q["enrollment_state"] == ["active"] and q["include[]"] == ["term"] and q["per_page"] == ["50"]
    assert TOKEN not in str(req.url)


def _paged(pages: list[list[dict[str, Any]]], path: str) -> Callable[[httpx.Request], httpx.Response]:
    def handler(request: httpx.Request) -> httpx.Response:
        n = int(dict(urllib.parse.parse_qsl(request.url.query.decode())).get("page", "1"))
        headers = {}
        if n < len(pages):
            headers["Link"] = (f'<{BASE}{path}?page={n}&per_page=50>; rel="current",'
                               f'<{BASE}{path}?page={n + 1}&per_page=50>; rel="next",'
                               f'<{BASE}{path}?page=1&per_page=50>; rel="first"')
        return httpx.Response(200, json=pages[n - 1], headers=headers)
    return handler


def test_pagination_follows_link_next_to_the_end() -> None:
    pages = [[COURSES[0]], [COURSES[1]], [COURSES[2]]]
    fake = Fake({"/api/v1/courses": _paged(pages, "/api/v1/courses")})
    out = fake.hub().call("canvas_courses", {})
    assert [c["id"] for c in out["courses"]] == [101, 102]
    assert len(fake.seen) == 3


def test_pagination_stops_at_the_cap(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setattr(canvas, "MAX_PAGES", 2)
    pages = [[COURSES[0]]] * 5
    fake = Fake({"/api/v1/courses": _paged(pages, "/api/v1/courses")})
    fake.hub().call("canvas_courses", {})
    assert len(fake.seen) == 2


def test_a_next_link_to_another_host_is_not_followed() -> None:
    def handler(request: httpx.Request) -> httpx.Response:
        return httpx.Response(200, json=[COURSES[0]],
                              headers={"Link": '<https://elsewhere.example.com/steal?page=2>; rel="next"'})
    fake = Fake({"/api/v1/courses": handler})
    out = fake.hub().call("canvas_courses", {})
    assert out["ok"] is False and "did not follow" in out["reason"]
    assert [r.url.host for r in fake.seen] == ["canvas.example.edu"]


def test_next_link_parsing() -> None:
    header = '<https://a.example/x?page=1>; rel="current", <https://a.example/x?page=2>; rel="next"'
    assert canvas.next_link(header) == "https://a.example/x?page=2"
    assert canvas.next_link('<https://a.example/x?page=1>; rel="first"') == ""
    assert canvas.next_link("") == ""


# ---- what's due, in the owner's zone ------------------------------------------------------------------------

ASSIGN_333 = [
    {"id": 1, "name": "HW2", "due_at": "2026-10-03T06:59:00Z", "points_possible": 50,
     "html_url": f"{BASE}/courses/101/assignments/1", "submission": {"workflow_state": "unsubmitted"}},
    {"id": 2, "name": "Ex 5", "due_at": "2026-10-01T17:30:00Z", "points_possible": 3,
     "html_url": f"{BASE}/courses/101/assignments/2",
     "submission": {"workflow_state": "submitted", "submitted_at": "2026-09-29T20:00:00Z"}},
    {"id": 3, "name": "Ex 4 (late)", "due_at": "2026-09-28T17:30:00Z",
     "html_url": f"{BASE}/courses/101/assignments/3", "submission": {"workflow_state": "unsubmitted", "missing": True}},
    {"id": 4, "name": "Ex 3 done", "due_at": "2026-09-26T17:30:00Z",
     "html_url": f"{BASE}/courses/101/assignments/4",
     "submission": {"workflow_state": "graded", "submitted_at": "2026-09-25T20:00:00Z", "grade": "3"}},
    {"id": 5, "name": "Final project", "due_at": "2026-11-20T07:59:00Z", "html_url": f"{BASE}/courses/101/assignments/5"},
    {"id": 6, "name": "Participation", "due_at": None, "html_url": f"{BASE}/courses/101/assignments/6"},
]


def test_due_lists_the_window_in_local_time_with_submission_status() -> None:
    fake = Fake(routes(**{"/api/v1/courses/101/assignments": ASSIGN_333,
                          "/api/v1/courses/102/assignments": httpx.Response(
                              403, json={"errors": [{"message": "forbidden"}]})}))
    out = fake.hub().call("canvas_due", {"days": None, "course": None})
    assert out["ok"]
    names = [i["name"] for i in out["items"]]
    assert names == ["Ex 4 (late)", "Ex 5", "HW2"]              # by due time; done-and-past, far and undated left out
    hw2 = out["items"][-1]
    assert hw2 == {"course": "CSE 333 A", "name": "HW2", "due": "Fri Oct 2, 11:59 PM PDT",
                   "due_at": "2026-10-03T06:59:00+00:00", "status": "not submitted", "points": 50,
                   "url": f"{BASE}/courses/101/assignments/1"}
    assert out["items"][0]["status"] == "missing" and out["items"][1]["status"] == "submitted"
    assert out["could_not_read"] == ["CSE 447 A"]               # one course refusing does not sink the answer
    q = urllib.parse.parse_qs(fake.seen[1].url.query.decode())
    assert q["include[]"] == ["submission"] and q["order_by"] == ["due_at"]


def test_due_times_follow_the_zone() -> None:
    fake = Fake(routes(**{"/api/v1/courses/101/assignments": ASSIGN_333[:1],
                          "/api/v1/courses/102/assignments": []}))
    out = fake.hub(tz=ZoneInfo("America/New_York")).call("canvas_due", {"days": 7, "course": None})
    assert out["items"][0]["due"] == "Sat Oct 3, 2:59 AM EDT"


def test_due_for_one_course_and_a_longer_window() -> None:
    fake = Fake(routes(**{"/api/v1/courses/101/assignments": ASSIGN_333}))
    out = fake.hub().call("canvas_due", {"days": 90, "course": "cse333"})
    assert out["days"] == canvas.MAX_DAYS and "Final project" in [i["name"] for i in out["items"]]
    assert {r.url.path for r in fake.seen} == {"/api/v1/courses", "/api/v1/courses/101/assignments"}
    assert fake.hub().call("canvas_due", {"days": 7, "course": "chem 142"})["ok"] is False


# ---- errors ----------------------------------------------------------------------------------------------------

def test_a_refused_token_says_how_to_make_a_new_one() -> None:
    fake = Fake({"/api/v1/courses": httpx.Response(401, json={"errors": [{"message": "Invalid access token."}]},
                                                    headers={"WWW-Authenticate": 'Bearer realm="canvas-lms"'})})
    for name, args in (("canvas_courses", {}), ("canvas_due", {"days": 7, "course": None})):
        out = fake.hub().call(name, args)
        assert out["ok"] is False and out["line"] == canvas.TOKEN_LINE
        assert "New Access Token" in out["line"] and "CANVAS_API_TOKEN" in out["line"]


def test_canvas_permission_401_is_a_course_refusal_not_a_dead_token() -> None:
    fake = Fake(routes(**{"/api/v1/courses/101/assignments": ASSIGN_333[:1],
                          "/api/v1/courses/102/assignments": httpx.Response(
                              401, json={"status": "unauthorized",
                                         "errors": [{"message": "user not authorized to perform that action"}]})}))
    out = fake.hub().call("canvas_due", {"days": 7, "course": None})
    assert out["ok"] and out["could_not_read"] == ["CSE 447 A"]


def test_throttle_and_timeout_are_plain_lines() -> None:
    fake = Fake({"/api/v1/courses": httpx.Response(403, text="403 Forbidden (Rate Limit Exceeded)")})
    assert fake.hub().call("canvas_courses", {})["line"] == canvas.RATE_LINE

    def slow(request: httpx.Request) -> httpx.Response:
        raise httpx.ReadTimeout("slow", request=request)
    out = Fake({"/api/v1/courses": slow}).hub().call("canvas_courses", {})
    assert out["ok"] is False and "in time" in out["reason"]


# ---- announcements and links -------------------------------------------------------------------------------------

def test_announcements_are_text_newest_first_with_links() -> None:
    rows = [
        {"id": 7, "title": "Room change", "posted_at": "2026-09-28T16:00:00Z", "context_code": "course_102",
         "message": "<p>Lecture moves to <b>CSE2 G10</b> &amp; stays there.</p>",
         "html_url": f"{BASE}/courses/102/discussion_topics/7"},
        {"id": 8, "title": "HW2 out", "posted_at": "2026-09-29T16:00:00Z", "context_code": "course_101",
         "message": "<div>See the spec.</div>", "html_url": f"{BASE}/courses/101/discussion_topics/8"},
    ]
    fake = Fake(routes(**{"/api/v1/announcements": rows}))
    out = fake.hub().call("canvas_announcements", {"course": None, "days": None})
    assert [i["title"] for i in out["items"]] == ["HW2 out", "Room change"]
    assert out["items"][1]["text"] == "Lecture moves to CSE2 G10 & stays there."
    assert out["items"][1]["course"] == "CSE 447 A" and out["items"][0]["posted"] == "Tue Sep 29, 9:00 AM PDT"
    q = urllib.parse.parse_qs(fake.seen[-1].url.query.decode())
    assert q["context_codes[]"] == ["course_101", "course_102"] and q["start_date"] == ["2026-09-16"]
    out = fake.hub().call("canvas_announcements", {"course": "447", "days": 3})
    assert urllib.parse.parse_qs(fake.seen[-1].url.query.decode())["context_codes[]"] == ["course_102"]


def test_find_link_across_kinds() -> None:
    fake = Fake(routes(**{
        "/api/v1/courses/102/assignments": [{"id": 1, "name": "A3 n-grams",
                                             "html_url": f"{BASE}/courses/102/assignments/1"}],
        "/api/v1/courses/102/pages": [{"page_id": 5, "url": "a3-guide", "title": "A3 guide"}],
        "/api/v1/courses/102/files": [{"id": 77, "display_name": "a3-starter.zip",
                                       "url": f"{BASE}/files/77/download?download_frd=1&verifier=placeholder"}],
        "/api/v1/courses/102/modules": [{"id": 9, "name": "Week 3: A3", "items": [
            {"title": "Week 3", "type": "SubHeader"},
            {"title": "Slides", "type": "File", "html_url": f"{BASE}/courses/102/modules/items/1"}]}],
    }))
    out = fake.hub().call("canvas_find_link", {"course": "CSE 447 A", "query": "a3", "kind": "any"})
    assert out["ok"] and out["course"] == "CSE 447 A"
    by_kind = {link["kind"]: link for link in out["links"]}
    assert by_kind["assignment"]["url"] == f"{BASE}/courses/102/assignments/1"
    assert by_kind["page"]["url"] == f"{BASE}/courses/102/pages/a3-guide"
    assert by_kind["file"]["url"] == f"{BASE}/courses/102/files/77"      # the file's page, never its download url
    assert "verifier" not in json.dumps(out)
    assert by_kind["module"]["items"] == [{"title": "Slides", "type": "File",
                                           "url": f"{BASE}/courses/102/modules/items/1"}]
    searched = [urllib.parse.parse_qs(r.url.query.decode()).get("search_term") for r in fake.seen[1:]]
    assert searched == [["a3"]] * 4
    # one kind only
    fake.seen.clear()
    out = fake.hub().call("canvas_find_link", {"course": "447", "query": "a3", "kind": "page"})
    assert [link["kind"] for link in out["links"]] == ["page"]
    assert [r.url.path for r in fake.seen] == ["/api/v1/courses", "/api/v1/courses/102/pages"]
    # files hidden from students: the rest still answers
    fake.routes["/api/v1/courses/102/files"] = httpx.Response(403, json={"errors": [{"message": "forbidden"}]})
    out = fake.hub().call("canvas_find_link", {"course": "447", "query": "a3", "kind": "any"})
    assert out["ok"] and out["could_not_read"] == ["files"]
    # an unclear course names the choices
    out = fake.hub().call("canvas_find_link", {"course": "CSE", "query": "a3", "kind": "any"})
    assert out["ok"] is False and out["courses"] == ["CSE 333 A", "CSE 447 A"]


# ---- read only ---------------------------------------------------------------------------------------------------

def test_only_gets_leave_and_a_post_is_refused_before_the_transport() -> None:
    fake = Fake(routes(**{"/api/v1/courses/101/assignments": ASSIGN_333, "/api/v1/courses/102/assignments": [],
                          "/api/v1/announcements": []}))
    hub = fake.hub()
    for name, args in (("canvas_courses", {}), ("canvas_due", {"days": 7, "course": None}),
                       ("canvas_announcements", {"course": None, "days": 14}),
                       ("canvas_find_link", {"course": "333", "query": "hw", "kind": "any"})):
        hub.call(name, args)
    assert fake.seen and {r.method for r in fake.seen} == {"GET"}
    # positive control: the guard catches a write, and the fake never sees it
    before = len(fake.seen)
    with pytest.raises(canvas.WriteRefused):
        hub.api._client.post(f"{BASE}/api/v1/courses/101/assignments/1/submissions", json={})
    for method in ("PUT", "DELETE", "PATCH"):
        with pytest.raises(canvas.WriteRefused):
            hub.api._client.request(method, f"{BASE}/api/v1/courses/101")
    assert len(fake.seen) == before


def test_rundown_part_is_short_and_only_open_work() -> None:
    fake = Fake(routes(**{"/api/v1/courses/101/assignments": ASSIGN_333, "/api/v1/courses/102/assignments": []}))
    part = fake.hub().rundown()
    assert part["available"] and [i["name"] for i in part["items"]] == ["Ex 4 (late)", "HW2"]
    assert set(part["items"][0]) == {"course", "name", "due", "status", "url"}
    bad = Fake({"/api/v1/courses": httpx.Response(401, json={})}).hub().rundown()
    assert bad == {"available": False, "reason": canvas.TOKEN_LINE}


# ---- wiring: the Telegram brain, the rundown, self context -----------------------------------------------------

class Hub:
    def __init__(self) -> None:
        self.handled: list = []

    def tools(self) -> list:
        return canvas.TOOLS

    def instructions(self) -> str:
        return canvas.INSTRUCTIONS

    async def handle(self, name: str, args: dict) -> dict:
        self.handled.append((name, args))
        return {"ok": True, "items": []}

    def rundown(self) -> dict:
        return {"available": True, "due_next_days": 3, "items": [{"course": "CSE 333 A", "name": "HW2"}]}


def test_the_telegram_brain_gets_the_tools_only_when_canvas_is_on() -> None:
    hub = Hub()
    args = {"days": None, "course": None}
    rig = Rig(FakeApi(), FakeCreate(call("canvas_due", args), say("Two things.")), canvas=hub)
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "what's due this week", message_id=9)))
    first = rig.create.requests[0]
    assert set(canvas.TOOL_NAMES) <= {t.get("name") for t in first["tools"]}
    assert "canvas_due" in first["instructions"]
    assert hub.handled == [("canvas_due", args)]
    rig = Rig(FakeApi(), FakeCreate(say("ok")))
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "hi", message_id=10)))
    assert not any(t.get("name") in canvas.TOOL_NAMES for t in rig.create.requests[0]["tools"])
    assert "canvas" not in rig.create.requests[0]["instructions"].lower()


def test_the_rundown_carries_canvas_deadlines_only_when_on(tmp_path: Any) -> None:
    now = datetime.now().astimezone()
    assert '"canvas"' not in rundown.context(None, now)
    text = rundown.context(None, now, canvas={"available": True, "items": [{"name": "HW2"}]})
    assert '"canvas": {"available": true' in text
    rig = Rig(FakeApi(), FakeCreate(say("Your day.")), canvas=Hub())
    asyncio.run(rig.inlet._turn(telegram.Inbound(OWNER, OWNER, "rundown", message_id=11)))
    sent = json.dumps(rig.create.requests[0])
    assert "HW2" in sent
    assert not any(t.get("name") in canvas.TOOL_NAMES for t in rig.create.requests[0]["tools"])   # still read by code


def test_self_context_names_canvas_only_when_on() -> None:
    env = {"OPENROUTER_API_KEY": "test-openai", "EVE_TELEGRAM": "1", "EVE_TELEGRAM_TOKEN": "test-token",
           "EVE_TELEGRAM_OWNER": "1"}
    assert not any("Canvas" in ln for ln in self_context.lines(env, commit=""))
    on = self_context.lines({**env, **ENV}, commit="")
    assert "Canvas (school courses): on, read only." in on
    assert BASE not in "\n".join(on)
