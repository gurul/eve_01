"""The direct apps (apps_mcp.py, google_rest.py): policy, schemas, the MCP client, tokens and Google's APIs.

Every HTTP exchange goes through httpx.MockTransport, so the real request code runs and nothing leaves the Mac.
"""
from __future__ import annotations

import base64
import json
import logging
import os
import stat
import time
from pathlib import Path
from typing import Any, Callable

import httpx
import pytest

from eve_bridge import apps_mcp, google_rest
from eve_bridge.apps_mcp import AppsBridge, AppsConfig, McpClient, McpError


def cfg(tmp_path: Path, environ: dict[str, str] | None = None, **policy: str) -> AppsConfig:
    env = dict(environ or {})
    if policy:
        env["EVE_APPS_POLICY"] = ",".join(f"{k}={v}" for k, v in policy.items())
    return AppsConfig(enabled=True, config_dir=tmp_path, policy=apps_mcp.policy(env), environ=env)


# ---- pure: reading or writing, schemas, decisions ------------------------------------------------------

@pytest.mark.parametrize(("tool", "reads"), [
    ({"name": "search_threads", "annotations": {"readOnlyHint": True}}, True),
    ({"name": "trash_thread", "annotations": {"readOnlyHint": True}}, False),     # a hint never makes a trash a read
    ({"name": "create_event", "annotations": {"readOnlyHint": False}}, False),
    ({"name": "list_events", "annotations": {"readOnlyHint": False}}, False),     # the server says it writes
    ({"name": "get_draft", "annotations": {"readOnlyHint": True}}, True),        # draft as a noun
    ({"name": "draft_reply"}, False),                                            # draft as the verb
    ({"name": "lookup_weather", "annotations": {"readOnlyHint": True}}, True),
    ({"name": "lookup_weather"}, False),                                         # no hint is not a read
    ({"name": "find_and_replace"}, False),                                       # Lean AppsPolicy P1
    ({"name": "push_files"}, False),
    ({"name": "issue_write"}, False),
    ({"name": "ui_get", "annotations": {"readOnlyHint": True}}, True),
    ({"name": "frobnicate"}, False),                                             # no reading verb: asks
])
def test_a_tool_reads_only_when_its_hint_and_its_name_agree(tool: dict[str, Any], reads: bool) -> None:
    assert apps_mcp.tool_reads(tool) is reads


def test_the_policy_has_the_owners_defaults_and_an_env_override() -> None:
    # Private routine writes run; consequential acts still ask. Contacts and Maps only look.
    assert apps_mcp.policy({}) == {"gmail": "auto", "calendar": "auto", "drive": "auto", "contacts": "read",
                                   "github": "auto", "maps": "read"}
    pol = apps_mcp.policy({"EVE_APPS_POLICY": "github=write, drive=read, maps=never, =x"})
    assert pol["github"] == "write" and pol["drive"] == "read" and pol["maps"] == "read"   # "never" ignored


def test_scopes_follow_the_policy() -> None:
    g = "https://www.googleapis.com/auth/"
    contacts = [g + "contacts.readonly", g + "contacts.other.readonly"]
    # sending is gmail.send, which cannot read, delete or label anything; never gmail.modify
    assert apps_mcp.google_scopes(apps_mcp.policy({})) == [
        g + "gmail.readonly", g + "gmail.send", g + "calendar", g + "drive.readonly", g + "drive.file"] + contacts
    strict = apps_mcp.google_scopes({"gmail": "read", "calendar": "read", "drive": "read"})
    assert strict == [g + "gmail.readonly", g + "calendar.readonly", g + "drive.readonly"] + contacts


def test_a_schema_is_slimmed_for_the_model() -> None:
    raw = {"type": "object", "title": "Req", "x-google-thing": 1, "properties": {
        "q": {"type": "string", "description": "word " * 100, "x-google-enum-descriptions": ["a"]},
        "old": {"type": "string", "deprecated": True},
        "nested": {"type": "array", "items": {"type": "object", "title": "T", "properties": {"a": {"type": "integer"}}}}}}
    slim = apps_mcp.slim_schema(raw)
    assert set(slim) == {"type", "properties"} and set(slim["properties"]) == {"q", "nested"}
    assert len(slim["properties"]["q"]["description"]) == apps_mcp.PROP_CLIP
    assert "x-google-enum-descriptions" not in slim["properties"]["q"]
    assert slim["properties"]["nested"]["items"] == {"type": "object", "properties": {"a": {"type": "integer"}}}
    fn = apps_mcp.function_tool("calendar", {"name": "list_events", "description": "Events.", "inputSchema": raw})
    assert fn["name"] == "calendar__list_events" and fn["strict"] is False
    assert fn["description"] == "[Google Calendar] Events." and fn["parameters"]["type"] == "object"
    bare = apps_mcp.function_tool("maps", {"name": "x"})
    assert bare["parameters"] == {"type": "object", "properties": {}}


def test_the_confirmation_line_is_plain_and_clipped() -> None:
    line = apps_mcp.describe_for_confirmation("github__create_issue", {"repo": "eve", "title": "t" * 200})
    assert line.startswith("GitHub: create issue with repo: eve, title: ttt") and line.endswith("?")
    assert len(line) <= apps_mcp.LINE_CLIP
    assert apps_mcp.describe_for_confirmation("drive__copy_file", {}) == "Google Drive: copy file?"


def test_a_result_reaches_the_model_structured_or_as_text_and_clipped() -> None:
    assert apps_mcp.result_for_model({"structuredContent": {"a": 1}}) == {"ok": True, "result": {"a": 1}}
    text = apps_mcp.result_for_model({"content": [{"type": "text", "text": "one"}, {"type": "image", "data": "x"},
                                                  {"type": "text", "text": "two"}]})
    assert text == {"ok": True, "result": "one\ntwo", "not_shown": ["image"]}
    err = apps_mcp.result_for_model({"isError": True, "content": [{"type": "text", "text": "API disabled"}]})
    assert err["ok"] is False and err["reason"] == "API disabled"
    big = apps_mcp.result_for_model({"content": [{"type": "text", "text": "x" * (apps_mcp.RESULT_CLIP + 10)}]})
    assert big["clipped"] and len(big["result"]) == apps_mcp.RESULT_CLIP + 1


# ---- the bridge over fake servers ------------------------------------------------------------------------

TOOLS = {
    "gmail": [{"name": "search_threads", "annotations": {"readOnlyHint": True}},
              {"name": "trash_thread", "annotations": {"readOnlyHint": False}}],
    "calendar": [{"name": "list_events", "annotations": {"readOnlyHint": True}},
                 {"name": "create_event", "annotations": {"readOnlyHint": False}}],
    "drive": [{"name": "search_files", "annotations": {"readOnlyHint": True}},
              {"name": "create_file", "annotations": {"readOnlyHint": False}}],
    "github": [{"name": "list_issues", "annotations": {"readOnlyHint": True}}, {"name": "create_issue"}],
    "maps": [{"name": "compute_routes", "annotations": {"readOnlyHint": True}}],
    "contacts": [{"name": "search_contacts", "annotations": {"readOnlyHint": True}},
                 {"name": "delete_contact", "annotations": {"readOnlyHint": False}}],
}


class Fake:
    def __init__(self, app: str, calls: list[Any], fail: bool = False, result: Any = None) -> None:
        self.app, self.calls, self.fail, self.result = app, calls, fail, result

    def list_tools(self) -> list[dict[str, Any]]:
        if self.fail:
            raise McpError("not signed in or not allowed (HTTP 403)")
        return [dict(t) for t in TOOLS[self.app]]

    def call_tool(self, name: str, args: Any) -> dict[str, Any]:
        self.calls.append((self.app, name, dict(args)))
        if isinstance(self.result, Exception):
            raise self.result
        return self.result or {"structuredContent": {"ok": name}}


def bridge(tmp_path: Path, fail: tuple[str, ...] = (), result: Any = None, **policy: str) -> tuple[AppsBridge, list]:
    calls: list[Any] = []
    b = AppsBridge(cfg(tmp_path, **policy),
                   client_factory=lambda s, tok: Fake(s.app, calls, s.app in fail, result),
                   rest_factory=lambda app, tok: Fake(app, calls, app in fail, result),
                   tokens={"google": lambda: "g", "github": lambda: "h", "maps": lambda: "m"})
    b.start()
    return b, calls


def test_start_offers_each_apps_tools_under_its_policy(tmp_path: Path) -> None:
    b, _ = bridge(tmp_path, gmail="read")
    names = {t["name"] for t in b.tools()}
    assert "gmail__search_threads" in names and "gmail__trash_thread" not in names      # read only: not offered
    assert "contacts__search_contacts" in names and "contacts__delete_contact" not in names   # contacts only look
    assert {"calendar__create_event", "drive__create_file", "github__create_issue"} <= names
    assert b.paths == {"gmail": "rest", "calendar": "rest", "drive": "rest", "contacts": "rest", "github": "mcp",
                       "maps": "mcp"}
    assert b.names == names | {apps_mcp.OPEN_TOOL}


def test_an_app_that_fails_is_left_out_and_the_rest_come_up(tmp_path: Path, caplog: Any) -> None:
    with caplog.at_level(logging.WARNING):
        b, _ = bridge(tmp_path, fail=("github",))
    assert "github" not in b.apps and "gmail" in b.apps
    assert "GitHub is not available" in caplog.text


def test_decisions_follow_the_policy(tmp_path: Path) -> None:
    b, _ = bridge(tmp_path)
    assert b.decide("gmail__search_threads").action == "run"
    assert b.decide("calendar__create_event", {"summary": "Focus"}).action == "run"
    assert b.decide("calendar__create_event").action == "ask"                 # args must establish no delivery
    assert b.decide("drive__create_file", {"title": "Private notes"}).action == "run"
    assert b.decide("github__create_issue").action == "ask"
    assert b.decide("github__list_issues").action == "run"
    assert b.decide("gmail__trash_thread").action == "ask"                     # Gmail asks by default
    assert b.decide("contacts__delete_contact").action == "refuse"             # never offered, never run
    strict, _ = bridge(tmp_path, calendar="read", gmail="read")
    assert "calendar__create_event" not in strict.names
    assert strict.decide("gmail__trash_thread").action == "refuse"


def test_open_app_loads_an_app_for_the_turn(tmp_path: Path) -> None:
    b, _ = bridge(tmp_path)
    opened: set[str] = set()
    first = b.tools_for(opened)
    assert [t["name"] for t in first] == [apps_mcp.OPEN_TOOL]
    apps_prop = first[0]["parameters"]["properties"]["apps"]
    assert apps_prop["type"] == "array" and apps_prop["minItems"] == 1
    assert set(apps_prop["items"]["enum"]) == set(b.apps) and first[0]["parameters"]["required"] == ["apps"]
    out = b.open({"app": "Calendar"}, opened)                                    # the older single-app form
    assert out["ok"] and out["opened"] == ["calendar"] and opened == {"calendar"}
    assert "calendar__create_event" in out["tools"]
    names = [t["name"] for t in b.tools_for(opened)]
    assert "calendar__list_events" in names and names[0] == apps_mcp.OPEN_TOOL
    assert "calendar" not in b.tools_for(opened)[0]["parameters"]["properties"]["apps"]["items"]["enum"]
    assert b.open({"app": "slack"}, opened)["ok"] is False
    assert b.tools_for(set(b.apps))[0]["name"] != apps_mcp.OPEN_TOOL           # all open: no open_app


def test_open_app_opens_several_apps_in_one_call_and_reports_the_unknown_ones(tmp_path: Path) -> None:
    b, _ = bridge(tmp_path)
    opened: set[str] = set()
    out = b.open({"apps": ["calendar", "Maps", "slack", "calendar"]}, opened)
    assert out["ok"] and out["opened"] == ["calendar", "maps"] and opened == {"calendar", "maps"}
    assert set(out["tools"]) == {"calendar__list_events", "calendar__create_event", "maps__compute_routes"}
    assert out["not_connected"] == ["slack"] and "slack is not connected" in out["note"]
    assert b.open({"apps": "github"}, opened)["opened"] == ["github"]           # a lone string is one app
    nothing = b.open({"apps": []}, opened)
    assert nothing["ok"] is False and "connected:" in nothing["reason"]


@pytest.mark.parametrize(("text", "apps"), [
    ("any mail from Sam?", {"gmail"}),
    ("what's on my calendar tomorrow", {"calendar"}),
    ("find the budget sheet in my drive", {"drive"}),
    ("open PRs on eve", {"github"}),
    ("how long to SFO by car", {"maps"}),
    ("what's the weather like", {"maps"}),
    # compound requests open every app they need at once: a miss is a whole extra round
    ("look for my next class on my calendar and give me a maps link", {"calendar", "maps"}),
    ("where's my next lecture", {"calendar", "maps"}),
    ("where\u2019s my midterm and how long to get there", {"calendar", "maps"}),
    ("when is my flight and what's the traffic like", {"calendar", "maps"}),
    ("did CI pass on eve", {"github"}),
    ("did the build pass on the main branch", {"github"}),
    ("any receipts in my inbox", {"gmail"}),
    ("did Sam's email get a reply, and add it to the spreadsheet", {"gmail", "drive"}),
    ("what's next", {"calendar"}),
    ("how does my day look", {"calendar"}),
    ("am I free at 3", {"calendar"}),
    ("rsvp yes to the seminar", {"calendar"}),
    ("office hours this week", {"calendar"}),
    ("coffee near me", {"maps"}),
    ("drive to the office", {"maps"}),                                         # driving there, not Google Drive
    ("open the pdf in my drive", {"drive"}),
    # negative controls: nothing opens on a plain sentence, a day word or "build me"
    ("I finally finished", set()),
    ("tell me a joke", set()),
    ("what's 2+2", set()),
    ("remind me tomorrow", set()),
    ("what about today", set()),
    ("build me an app for groceries", set()),
    ("what is my email address", {"gmail"}),                                   # an email address is not a place
    # an issue is GitHub's only when nothing else is its home (review, 2026-10-05)
    ("any open issues on eve?", {"github"}),
    ("file an issue on the eve repo", {"github"}),
    ("did my PR merge", {"github"}),
    ("did the actions run fail on main", {"github"}),
    ("add a linear issue for the login bug", set()),
    ("what are the open issues in linear", set()),
    ("any issues with my order on uber eats", set()),
    ("what actions do I need to take today", set()),
    ("merge these two notes", set()),
])
def test_a_message_opens_the_apps_it_names(tmp_path: Path, text: str, apps: set[str]) -> None:
    b, _ = bridge(tmp_path)
    assert b.guess(text) == apps


def test_owns_and_ensure_open_cover_every_up_apps_names(tmp_path: Path) -> None:
    b, _ = bridge(tmp_path, fail=("drive",))
    assert b.owns("calendar__list_events") and b.owns("calendar__anything_at_all")
    assert b.owns(apps_mcp.OPEN_TOOL)
    assert not b.owns("drive__search_files")                                    # drive is not up
    assert not b.owns("slack__send") and not b.owns("web_search") and not b.owns("calendar__")
    opened: set[str] = set()
    b.ensure_open("maps__compute_routes", opened)
    b.ensure_open("drive__search_files", opened)
    b.ensure_open("web_search", opened)
    assert opened == {"maps"}
    none_up = AppsBridge(cfg(tmp_path), tokens={})
    assert not none_up.owns(apps_mcp.OPEN_TOOL) and not none_up.owns("calendar__list_events")


def test_a_tool_an_app_lacks_is_refused_with_its_real_tools(tmp_path: Path) -> None:
    b, _ = bridge(tmp_path, gmail="read")
    made_up = b.decide("calendar__find_next_class")
    assert made_up.action == "refuse"
    assert "calendar__list_events" in made_up.why and "calendar__create_event" in made_up.why
    assert "Google Calendar" in made_up.why
    read_only = b.decide("gmail__trash_thread")                                 # a write a read policy never offered
    assert read_only.action == "refuse" and "read only" in read_only.why and "gmail__search_threads" in read_only.why
    assert "gmail__trash_thread" not in read_only.why.split("Its tools:")[1]
    assert b.decide("slack__send").action == "refuse"                           # not an app here at all


def test_execute_calls_the_servers_own_tool_and_never_raises(tmp_path: Path) -> None:
    b, calls = bridge(tmp_path)
    assert b.execute("calendar__list_events", {"pageSize": 3}) == {"ok": True, "result": {"ok": "list_events"}}
    assert calls[-1] == ("calendar", "list_events", {"pageSize": 3})
    assert b.execute("slack__send", {})["ok"] is False
    broken, _ = bridge(tmp_path, result=RuntimeError("boom with mail text"))
    out = broken.execute("gmail__search_threads", {"query": "from:sam"})
    assert out == {"ok": False, "reason": "RuntimeError: boom with mail text"}


def test_instructions_name_the_apps_up_and_their_rules(tmp_path: Path) -> None:
    b, _ = bridge(tmp_path)
    text = b.instructions()
    assert "is opened first" in text and "Google Contacts (contacts__…): read only" in text
    assert "Gmail (gmail__…): routine private changes run" in text
    assert "sees each one whole" in text and "never guess an address" in text
    # the owner's rule: no tool name in the block, since which tools a turn offers changes turn to turn (2026-10-05)
    assert "open_app" not in text and "__list" not in text and "web_search" not in text
    assert "Google Calendar (calendar__…): routine private changes run" in text
    assert "GitHub (github__…): routine private changes run" in text
    assert "sending, deleting, sharing, publishing and purchases are asked by code" in text
    empty = AppsBridge(cfg(tmp_path), tokens={})
    assert empty.instructions() == "" and empty.available() == []


def test_instructions_steer_the_brain_to_the_apps_and_to_chain_them(tmp_path: Path) -> None:
    b, _ = bridge(tmp_path)
    text = b.instructions()
    assert "rather than a task on the Mac's screen or a web search" in text
    assert "every app the request needs in one call" in text
    assert "Chain apps in the same turn without asking" in text
    assert "the calendar's event list from now" in text and "mapsUrl and directionsUrl" in text
    assert "needs no maps call" in text and "travel time, nearby places and weather" in text
    assert "Keep the links that results carry" in text
    assert len(text) < 2600                                                     # it rides every turn's prompt
    # it writes no tool name at all (the owner's rule: never a name that is not offered; which tools a turn offers
    # changes turn to turn with the router, 2026-10-05), only app prefixes like gmail__…
    import re

    assert set(re.findall(r"\b[a-z]+__[a-z_]+\b", text)) - {"app__tool"} == set()
    # an app that is not up is not written about
    no_cal, _ = bridge(tmp_path, fail=("calendar", "maps"))
    other = no_cal.instructions()
    assert "calendar__" not in other and "maps__" not in other and "Chain apps" in other


def test_an_app_without_its_sign_in_is_not_available(tmp_path: Path) -> None:
    b = AppsBridge(cfg(tmp_path, {"EVE_GITHUB_MCP_TOKEN": "x"}))
    assert b.available() == ["github"]                                          # no Google token, no Maps key


def test_configured_reads_the_switch(tmp_path: Path) -> None:
    assert apps_mcp.configured({}).enabled is False
    on = apps_mcp.configured({"EVE_APPS": "1", "EVE_APPS_TIMEOUT_SECS": "-3",
                              "EVE_APPS_DIR": str(tmp_path)})
    assert on.enabled and on.timeout_secs == apps_mcp.DEFAULT_TIMEOUT_SECS and on.google_token_path.parent == tmp_path


# ---- the MCP client over a mock transport --------------------------------------------------------------

def mcp_server(handler: Callable[[dict[str, Any], httpx.Request], httpx.Response]) -> tuple[httpx.Client, list]:
    seen: list[httpx.Request] = []

    def respond(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        return handler(json.loads(request.content), request)

    return httpx.Client(transport=httpx.MockTransport(respond)), seen


def test_the_client_initializes_keeps_the_session_and_reads_sse(tmp_path: Path) -> None:
    def handler(msg: dict[str, Any], req: httpx.Request) -> httpx.Response:
        if msg["method"] == "initialize":
            return httpx.Response(200, json={"jsonrpc": "2.0", "id": msg["id"], "result": {}},
                                  headers={"Mcp-Session-Id": "s1"})
        if "id" not in msg:
            return httpx.Response(202)
        assert req.headers["mcp-session-id"] == "s1"
        body = (": ping\n\nevent: message\ndata: " + json.dumps(
            {"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": [{"name": "a"}], "nextCursor": "c2"}
            if not msg.get("params", {}).get("cursor") else {"tools": [{"name": "b"}]}}) + "\n\n")
        return httpx.Response(200, text=body, headers={"content-type": "text/event-stream"})

    http, seen = mcp_server(handler)
    client = McpClient("https://x/mcp", lambda: "tok", {"X-MCP-Toolsets": "repos"}, http=http)
    assert [t["name"] for t in client.list_tools()] == ["a", "b"]
    assert [json.loads(r.content)["method"] for r in seen] == [
        "initialize", "notifications/initialized", "tools/list", "tools/list"]
    assert seen[0].headers["authorization"] == "Bearer tok" and seen[0].headers["x-mcp-toolsets"] == "repos"
    assert seen[0].headers["mcp-protocol-version"] == apps_mcp.PROTOCOL_VERSION


def test_the_client_reopens_a_forgotten_session_once(tmp_path: Path) -> None:
    state = {"inits": 0, "calls": 0}

    def handler(msg: dict[str, Any], req: httpx.Request) -> httpx.Response:
        if msg["method"] == "initialize":
            state["inits"] += 1
            return httpx.Response(200, json={"jsonrpc": "2.0", "id": msg["id"], "result": {}},
                                  headers={"Mcp-Session-Id": f"s{state['inits']}"})
        if "id" not in msg:
            return httpx.Response(202)
        state["calls"] += 1
        if req.headers.get("mcp-session-id") == "s1":
            return httpx.Response(404)
        return httpx.Response(200, json={"jsonrpc": "2.0", "id": msg["id"], "result": {"content": []}})

    http, _ = mcp_server(handler)
    assert McpClient("https://x/mcp", lambda: None, http=http).call_tool("t", {}) == {"content": []}
    assert state == {"inits": 2, "calls": 2}


def test_the_client_sends_an_api_key_in_its_header_and_reports_errors() -> None:
    def handler(msg: dict[str, Any], req: httpx.Request) -> httpx.Response:
        if msg["method"] == "initialize":
            return httpx.Response(200, json={"jsonrpc": "2.0", "id": msg["id"], "result": {}})
        if "id" not in msg:
            return httpx.Response(202)
        assert req.headers["x-goog-api-key"] == "key" and "authorization" not in req.headers
        if msg["method"] == "tools/list":
            return httpx.Response(403)
        return httpx.Response(200, json={"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32602, "message": "bad"}})

    http, _ = mcp_server(handler)
    client = McpClient("https://x/mcp", lambda: "key", http=http, key_header="X-Goog-Api-Key")
    with pytest.raises(McpError, match="HTTP 403"):
        client.list_tools()
    with pytest.raises(McpError, match="bad"):
        client.call_tool("t", {})


def test_sse_parsing_joins_data_lines_and_skips_junk() -> None:
    body = 'data: {"id": 1,\ndata:  "result": {}}\n\ndata: not json\n\nevent: x\ndata: {"id": 2}\n'
    assert apps_mcp._sse_messages(body) == [{"id": 1, "result": {}}, {"id": 2}]


# ---- tokens --------------------------------------------------------------------------------------------

def google_files(tmp_path: Path, expires_at: float) -> None:
    (tmp_path / "google-oauth-client.json").write_text(json.dumps(
        {"installed": {"client_id": "cid", "client_secret": "sec"}}))
    apps_mcp.write_private_json(tmp_path / "google-token.json", {
        "refresh_token": "r1", "access_token": "a1", "expires_at": expires_at})


def test_a_google_token_is_refreshed_only_near_expiry_and_kept_private(tmp_path: Path) -> None:
    posts: list[dict[str, str]] = []

    def respond(request: httpx.Request) -> httpx.Response:
        posts.append(dict(httpx.QueryParams(request.content.decode())))
        return httpx.Response(200, json={"access_token": "a2", "expires_in": 3600})

    http = httpx.Client(transport=httpx.MockTransport(respond))
    google_files(tmp_path, time.time() + 3000)
    src = apps_mcp.TokenSource("google", cfg(tmp_path), http=http)
    assert src.available and src() == "a1" and posts == []
    google_files(tmp_path, time.time() + 10)
    assert src() == "a2"
    assert posts == [{"grant_type": "refresh_token", "refresh_token": "r1", "client_id": "cid",
                      "client_secret": "sec"}]
    saved = json.loads((tmp_path / "google-token.json").read_text())
    assert saved["access_token"] == "a2" and saved["refresh_token"] == "r1"
    assert stat.S_IMODE(os.stat(tmp_path / "google-token.json").st_mode) == 0o600
    assert "a2" not in repr(src)


def test_a_refused_refresh_is_none_and_a_log_line_without_the_token(tmp_path: Path, caplog: Any) -> None:
    http = httpx.Client(transport=httpx.MockTransport(
        lambda r: httpx.Response(400, json={"error": "invalid_grant"})))
    google_files(tmp_path, 0)
    with caplog.at_level(logging.WARNING):
        assert apps_mcp.TokenSource("google", cfg(tmp_path), http=http)() is None
    assert "invalid_grant" in caplog.text and "r1" not in caplog.text


def test_env_tokens_are_read_as_they_are(tmp_path: Path) -> None:
    c = cfg(tmp_path, {"EVE_GITHUB_MCP_TOKEN": " ghp \n", "EVE_GOOGLE_MAPS_KEY": ""})
    assert apps_mcp.TokenSource("github", c)() == "ghp" and apps_mcp.TokenSource("maps", c)() is None
    assert not apps_mcp.TokenSource("maps", c).available


def test_app_urls_stay_out_of_the_log() -> None:
    def record(msg: str) -> logging.LogRecord:
        return logging.LogRecord("httpx", logging.INFO, "", 0, msg, None, None)

    assert not apps_mcp._quiet_app_urls(record('HTTP Request: GET https://gmail.googleapis.com/gmail/v1/users/me/'
                                               'threads?q=from%3Asam "HTTP/1.1 200 OK"'))
    assert apps_mcp._quiet_app_urls(record('HTTP Request: POST https://api.openai.com/v1/responses "HTTP/1.1 200 OK"'))
    assert apps_mcp._quiet_app_urls in logging.getLogger("httpx").filters


# ---- Google's plain APIs -------------------------------------------------------------------------------

def b64(text: str) -> str:
    import base64

    return base64.urlsafe_b64encode(text.encode()).decode().rstrip("=")


def google(routes: Callable[[httpx.Request], Any], app: str) -> tuple[google_rest.GoogleRestClient, list]:
    seen: list[httpx.Request] = []

    def respond(request: httpx.Request) -> httpx.Response:
        seen.append(request)
        out = routes(request)
        return out if isinstance(out, httpx.Response) else httpx.Response(200, json=out)

    return google_rest.GoogleRestClient(app, lambda: "tok", http=httpx.Client(
        transport=httpx.MockTransport(respond))), seen


def test_gmail_search_summarises_each_thread() -> None:
    def routes(r: httpx.Request) -> Any:
        if r.url.path.endswith("/threads"):
            assert r.url.params["q"] == "from:sam" and r.url.params["maxResults"] == "2"
            return {"threads": [{"id": "t1"}], "nextPageToken": "n"}
        assert r.url.params.get_list("metadataHeaders") == ["From", "Subject", "Date"]
        return {"messages": [
            {"labelIds": ["INBOX"], "payload": {"headers": [{"name": "Subject", "value": "Lunch"}]}},
            {"labelIds": ["UNREAD"], "snippet": "noon?", "payload": {"headers": [
                {"name": "From", "value": "Sam"}, {"name": "Date", "value": "Sat"}]}}]}

    client, seen = google(routes, "gmail")
    out = client.call_tool("search_threads", {"query": "from:sam", "pageSize": 2})
    assert out["isError"] is False and seen[0].headers["authorization"] == "Bearer tok"
    assert out["structuredContent"] == {"threads": [{"threadId": "t1", "subject": "Lunch", "from": "Sam",
                                                     "date": "Sat", "messages": 2, "unread": True,
                                                     "snippet": "noon?"}], "nextPageToken": "n"}


def _sent(request: httpx.Request) -> Any:
    """The email a send request carries, parsed back from its base64url raw."""
    import email
    from email.policy import default

    raw = json.loads(request.content)["raw"]
    return email.message_from_bytes(base64.urlsafe_b64decode(raw + "=" * (-len(raw) % 4)), policy=default)


def test_gmail_send_builds_the_email_and_posts_it() -> None:
    def routes(r: httpx.Request) -> Any:
        assert r.method == "POST" and r.url.path.endswith("/messages/send")
        return {"id": "m9", "threadId": "t9"}

    client, seen = google(routes, "gmail")
    out = client.call_tool("send_message", {"to": ["ana@example.com"], "cc": "bo@example.com, cy@example.org",
                                            "subject": "Running late", "body": "Hi Ana,\n\n15 minutes late."})
    assert out == {"isError": False, "structuredContent": {"sent": True, "id": "m9", "threadId": "t9",
                                                           "to": ["ana@example.com"], "subject": "Running late"}}
    msg = _sent(seen[0])
    assert msg["To"] == "ana@example.com" and msg["Cc"] == "bo@example.com, cy@example.org"
    assert msg["Subject"] == "Running late" and msg["From"] is None           # Gmail fills in the account
    assert msg.get_content().replace("\r\n", "\n").strip() == "Hi Ana,\n\n15 minutes late."   # CRLF on the wire
    assert "threadId" not in json.loads(seen[0].content)


def test_gmail_reply_goes_into_the_thread_with_the_originals_ids() -> None:
    def routes(r: httpx.Request) -> Any:
        if r.method == "GET":
            assert r.url.path.endswith("/messages/orig1") and r.url.params["format"] == "metadata"
            return {"id": "orig1", "threadId": "th1", "payload": {"headers": [
                {"name": "Message-ID", "value": "<abc@mail.example>"},
                {"name": "References", "value": "<root@mail.example>"}, {"name": "Subject", "value": "Lunch?"}]}}
        return {"id": "m2", "threadId": "th1"}

    client, seen = google(routes, "gmail")
    out = client.call_tool("send_message", {"to": ["sam@example.com"], "body": "Noon works.",
                                            "replyToMessageId": "orig1"})
    assert out["structuredContent"]["subject"] == "Re: Lunch?"
    sent = json.loads(seen[1].content)
    assert sent["threadId"] == "th1"
    msg = _sent(seen[1])
    assert msg["In-Reply-To"] == "<abc@mail.example>"
    assert msg["References"] == "<root@mail.example> <abc@mail.example>"


def test_gmail_send_to_me_is_the_owners_own_address() -> None:
    def routes(r: httpx.Request) -> Any:
        if r.method == "GET":
            assert r.url.path.endswith("/users/me/profile")
            return {"emailAddress": "owner@example.com"}
        return {"id": "m3", "threadId": "t3"}

    client, seen = google(routes, "gmail")
    out = client.call_tool("send_message", {"to": "Me", "cc": ["ana@example.com"], "body": "hi"})
    assert out["structuredContent"]["to"] == ["owner@example.com"]
    assert _sent(seen[1])["To"] == "owner@example.com" and _sent(seen[1])["Cc"] == "ana@example.com"
    client, seen = google(lambda r: {"id": "m4"}, "gmail")
    client.call_tool("send_message", {"to": ["ana@example.com"], "body": "hi"})
    assert len(seen) == 1                                      # no profile read when "me" is not used


@pytest.mark.parametrize("args, why", [
    ({"to": ["ana@example.com\nBcc: eve@example.com"], "body": "x"}, "not an email address"),   # a header smuggled in
    ({"to": ["Ana"], "body": "x"}, "not an email address"),                                    # a name, not an address
    ({"to": [], "body": "x"}, "no recipient"),
    ({"to": ["ana@example.com"], "body": "  "}, "no body"),
    ({"to": ["ana@example.com"], "subject": "Hi\r\nBcc: eve@example.com", "body": "x"}, "one line"),
    ({"to": [f"p{i}@example.com" for i in range(21)], "body": "x"}, "more than 20"),
])
def test_gmail_send_refuses_what_is_not_plainly_an_email(args: dict[str, Any], why: str) -> None:
    client, seen = google(lambda r: {}, "gmail")
    out = client.call_tool("send_message", args)
    assert out["isError"] is True and why in out["content"][0]["text"] and seen == []      # nothing was sent


def test_contacts_search_warms_once_and_merges_saved_and_other_people() -> None:
    def routes(r: httpx.Request) -> Any:
        q = r.url.params["query"]
        if r.url.path.endswith("/people:searchContacts"):
            assert r.url.params["readMask"] == google_rest.CONTACT_FIELDS
            return {} if q == "" else {"results": [{"person": {
                "names": [{"displayName": "Ana Diaz"}], "emailAddresses": [{"value": "ana@example.com"}],
                "phoneNumbers": [{"value": "+1 555 0100"}], "organizations": [{"name": "UW"}]}}]}
        assert r.url.path.endswith("/otherContacts:search") and r.url.params["readMask"] == google_rest.OTHER_FIELDS
        return {} if q == "" else {"results": [
            {"person": {"names": [{"displayName": "Ana D"}], "emailAddresses": [{"value": "ANA@example.com"}]}},
            {"person": {"emailAddresses": [{"value": "ana.work@example.org"}]}}]}

    client, seen = google(routes, "contacts")
    out = client.call_tool("search_contacts", {"query": "ana"})
    assert out["structuredContent"] == {"people": [
        {"name": "Ana Diaz", "emails": ["ana@example.com"], "phones": ["+1 555 0100"], "organizations": ["UW"]},
        {"name": "", "emails": ["ana.work@example.org"], "saved": False}]}      # the same address twice is one person
    assert [r.url.params["query"] for r in seen] == ["", "", "ana", "ana"]       # Google's warm-up, then the search
    client.call_tool("search_contacts", {"query": "bo"})
    assert [r.url.params["query"] for r in seen[4:]] == ["bo", "bo"]             # warmed once


def test_contacts_is_a_read_only_toolset() -> None:
    assert [t["name"] for t in google_rest.TOOLS["contacts"]] == ["search_contacts"]
    assert all(apps_mcp.tool_reads(t) for t in google_rest.TOOLS["contacts"])
    send = next(t for t in google_rest.TOOLS["gmail"] if t["name"] == "send_message")
    assert not apps_mcp.tool_reads(send)                                         # a send is never a read


def test_an_email_question_is_the_whole_email_with_hidden_characters_shown() -> None:
    body = "Line one\n\nLine two " + "word " * 300
    q = apps_mcp.describe_whole("gmail__send_message", {"to": ["ana@example.com"], "subject": "Hi\u202e",
                                                        "body": body}, apps_mcp.WHOLE_ASK_CHARS)
    assert q.startswith("Send this email?\nTo: ana@example.com\nSubject: Hi[U+202E]\n\n") and q.endswith(body)
    assert apps_mcp.asks_whole("gmail__send_message") and not apps_mcp.asks_whole("drive__create_file")
    too_long = apps_mcp.describe_whole("gmail__send_message", {"to": ["a@b.co"], "body": "x" * 4000},
                                       apps_mcp.WHOLE_ASK_CHARS)
    assert too_long is None                                                     # refused unasked, never clipped


def test_mail_text_prefers_plain_and_falls_back_to_html() -> None:
    plain = {"mimeType": "multipart/alternative", "parts": [
        {"mimeType": "text/html", "body": {"data": b64("<p>html</p>")}},
        {"mimeType": "text/plain", "body": {"data": b64("plain text\n\n\nend")}}]}
    assert google_rest.mail_text(plain) == "plain text\nend"
    html = {"mimeType": "text/html", "body": {"data": b64("<style>x{}</style><p>Hi &amp; bye</p>")}}
    assert google_rest.mail_text(html) == "Hi & bye"
    long = {"mimeType": "text/plain", "body": {"data": b64("x" * (google_rest.BODY_CLIP + 5))}}
    assert len(google_rest.mail_text(long)) == google_rest.BODY_CLIP + 1


def test_calendar_lists_a_window_and_creates_with_a_meet_link() -> None:
    def routes(r: httpx.Request) -> Any:
        if r.url.path.endswith("/calendarList"):
            return {"items": []}                          # no list: list_events falls back to the primary
        if r.method == "GET":
            assert r.url.params["singleEvents"] == "true" and r.url.params["orderBy"] == "startTime"
            assert r.url.params["timeMax"].startswith("2026-10-12T09:00:00")         # start + 7 days
            return {"timeZone": "America/Los_Angeles", "items": [{"id": "e1", "summary": "Standup"}]}
        body = json.loads(r.content)
        assert r.url.params["conferenceDataVersion"] == "1" and r.url.params["sendUpdates"] == "all"
        assert body["start"] == {"dateTime": "2026-10-10T12:00:00-07:00", "timeZone": "America/Los_Angeles"}
        assert "date" not in body["end"]                                  # nothing to clear on a new event
        assert body["attendees"] == [{"email": "ana@example.com"}]
        assert body["conferenceData"]["createRequest"]["conferenceSolutionKey"] == {"type": "hangoutsMeet"}
        return {"id": "e2"}

    client, seen = google(routes, "calendar")
    listed = client.call_tool("list_events", {"startTime": "2026-10-05T09:00:00-07:00"})
    assert listed["structuredContent"]["events"][0]["summary"] == "Standup"
    made = client.call_tool("create_event", {"summary": "Lunch", "startTime": "2026-10-10T12:00:00-07:00",
                                             "endTime": "2026-10-10T13:00:00-07:00", "timeZone": "America/Los_Angeles",
                                             "attendeeEmails": ["ana@example.com"], "addGoogleMeetUrl": True})
    assert made["structuredContent"] == {"id": "e2"} and seen[-1].url.path.endswith("/calendars/primary/events")


@pytest.mark.parametrize(("location", "links"), [
    ("Soda Hall 306, Berkeley, CA", True),
    ("  Room 101  ", True),
    ("", False),
    (None, False),
    ("https://zoom.us/j/123456", False),
    ("meet.google.com/abc-defg-hij", False),
    ("abc-defg-hij", False),
    ("Zoom: zoom.us/j/99", False),
    ("Microsoft Teams Meeting", False),
    ("www.example.com", False),
])
def test_an_event_gets_maps_links_only_for_a_place(location: Any, links: bool) -> None:
    out = google_rest.compact_event({"id": "e", "summary": "CS 61A", "start": {"dateTime": "x"}, "location": location})
    assert ("mapsUrl" in out) is links and ("directionsUrl" in out) is links


def test_maps_links_follow_googles_url_scheme() -> None:
    links = google_rest.place_links("Soda Hall 306, Berkeley, CA")
    # developers.google.com/maps/documentation/urls/get-started: api=1, URL-encoded, spaces as %20
    assert links == {
        "mapsUrl": "https://www.google.com/maps/search/?api=1&query=Soda%20Hall%20306%2C%20Berkeley%2C%20CA",
        "directionsUrl": "https://www.google.com/maps/dir/?api=1&destination=Soda%20Hall%20306%2C%20Berkeley%2C%20CA"}
    assert google_rest.place_links("Caf\u00e9 & Bar #2")["mapsUrl"].endswith("query=Caf%C3%A9%20%26%20Bar%20%232")
    assert len(google_rest.place_links("x" * 5000)["directionsUrl"]) < 2048


def test_an_event_is_compacted_for_the_model_and_keeps_what_meet_reads() -> None:
    from eve_bridge import meet

    raw = {"kind": "calendar#event", "etag": "\"1\"", "id": "e1", "status": "confirmed", "htmlLink": "https://g/e1",
           "created": "x", "updated": "x", "summary": "Standup", "description": "Agenda. " + "y" * 900 +
           " join https://meet.google.com/abc-defg-hij now", "location": "Room 4, 1 Main St",
           "creator": {"email": "me@x"}, "organizer": {"email": "boss@x", "displayName": "Boss"},
           "start": {"dateTime": "2026-10-05T09:00:00-07:00"}, "end": {"dateTime": "2026-10-05T09:15:00-07:00"},
           "recurringEventId": "r1", "iCalUID": "u", "reminders": {"useDefault": True},
           "hangoutLink": "https://meet.google.com/abc-defg-hij",
           "conferenceData": {"entryPoints": [{"entryPointType": "video", "uri": "https://meet.google.com/abc-defg-hij"}]},
           "attendees": [{"email": "me@x", "self": True, "responseStatus": "accepted", "displayName": "Me"},
                         {"email": "boss@x", "organizer": True, "responseStatus": "accepted", "comment": "hi"}]}
    out = google_rest.compact_event(raw)
    assert set(out) == {"id", "status", "summary", "start", "end", "location", "mapsUrl", "directionsUrl",
                        "description", "hangoutLink", "conferenceData", "attendees", "organizer",
                        "recurringEventId", "htmlLink"}
    assert out["attendees"] == [{"email": "me@x", "responseStatus": "accepted", "self": True},
                                {"email": "boss@x", "responseStatus": "accepted", "organizer": True}]
    assert out["organizer"] == {"email": "boss@x"} and out["conferenceData"] == raw["conferenceData"]
    # clipped, but a call's link past the clip is kept, so Meet's lookup still finds it there
    assert out["description"].startswith("Agenda. ") and len(out["description"]) < 600
    assert out["description"].endswith("https://meet.google.com/abc-defg-hij")
    assert meet.events_in({"events": [out]}) == [out]
    assert meet.event_link(out) == meet.event_link(raw) == "https://meet.google.com/abc-defg-hij?hl=en"
    only_desc = google_rest.compact_event({"summary": "Sync", "start": {"dateTime": "x"},
                                           "description": "z" * 900 + " https://meet.google.com/abc-defg-hij"})
    assert meet.event_link(only_desc) == "https://meet.google.com/abc-defg-hij?hl=en"
    assert google_rest.compact_event({"id": "e2"}) == {"id": "e2"} and google_rest.compact_event("x") == "x"


def test_every_calendar_call_that_returns_events_compacts_them_with_links() -> None:
    raw = {"id": "e1", "etag": "x", "summary": "CS 61A", "location": "Wheeler 150",
           "start": {"dateTime": "2026-10-05T09:00:00-07:00"}}

    def routes(r: httpx.Request) -> Any:
        if r.url.path.endswith("/events") and r.method == "GET":
            return {"items": [raw]}
        if r.method == "GET":
            return {**raw, "attendees": [{"email": "me@x", "self": True}]}
        return raw

    client, _ = google(routes, "calendar")
    calls = {"list_events": {}, "search_events": {"query": "61A"}, "get_event": {"eventId": "e1"},
             "create_event": {"summary": "CS 61A", "startTime": "2026-10-05T09:00:00-07:00",
                              "endTime": "2026-10-05T10:00:00-07:00", "location": "Wheeler 150"},
             "update_event": {"eventId": "e1", "summary": "CS 61A"},
             "quick_add_event": {"text": "CS 61A Monday 9am at Wheeler 150"},
             "respond_to_event": {"eventId": "e1", "responseStatus": "accepted"}}
    for name, args in calls.items():
        out = client.call_tool(name, args)
        assert out["isError"] is False, name
        got = out["structuredContent"]
        events = got["events"] if "events" in got else [got]
        assert events[0]["mapsUrl"].endswith("query=Wheeler%20150"), name
        assert events[0]["directionsUrl"].endswith("destination=Wheeler%20150") and "etag" not in events[0], name


def test_all_day_events_use_dates() -> None:
    client, _ = google(lambda r: {}, "calendar")
    body = client._event_body({"summary": "Trip", "startTime": "2026-10-10", "endTime": "2026-10-12", "allDay": True})
    # dateTime null: a PATCH from a timed event clears it (nested objects merge)
    assert body == {"summary": "Trip", "start": {"date": "2026-10-10", "dateTime": None},
                    "end": {"date": "2026-10-12", "dateTime": None}}
    timed = client._event_body({"startTime": "2026-10-10T09:00:00-07:00"})
    assert timed["start"] == {"dateTime": "2026-10-10T09:00:00-07:00", "date": None}


def test_drive_search_quotes_plain_words_and_passes_a_drive_query_through() -> None:
    queries: list[str] = []

    def routes(r: httpx.Request) -> Any:
        queries.append(r.url.params["q"])
        return {"files": []}

    client, _ = google(routes, "drive")
    client.call_tool("search_files", {"query": "Ana's budget"})
    client.call_tool("search_files", {"query": "name contains 'budget'"})
    assert queries == ["fullText contains 'Ana\\'s budget' and trashed = false",
                       "name contains 'budget' and trashed = false"]


def test_drive_reads_a_doc_by_export_and_says_when_it_cannot() -> None:
    def routes(r: httpx.Request) -> Any:
        if r.url.path.endswith("/export"):
            assert r.url.params["mimeType"] == "text/markdown"           # headings and tables kept
            return httpx.Response(200, text="The plan.", headers={"content-type": "text/markdown"})
        mime = "application/zip" if "zip" in r.url.path else "application/vnd.google-apps.document"
        return {"name": "Plan", "mimeType": mime}

    client, _ = google(routes, "drive")
    doc = client.call_tool("read_file_content", {"fileId": "d1"})["structuredContent"]
    assert doc == {"name": "Plan", "mimeType": "application/vnd.google-apps.document", "text": "The plan.",
                   "clipped": False}
    zipped = client.call_tool("read_file_content", {"fileId": "zip1"})["structuredContent"]
    assert zipped["text"] == "" and "not read as text" in zipped["note"]


def test_drive_stops_reading_a_file_at_the_download_cap(monkeypatch: pytest.MonkeyPatch) -> None:
    # The cap was checked after r.content had read the whole body into memory; now the read stops at it
    monkeypatch.setattr(google_rest, "DOWNLOAD_MAX", 1024 * 1024)
    pulled: list[int] = []

    class Endless(httpx.SyncByteStream):
        def __iter__(self):
            for _ in range(100):                                     # 100 MB if read to the end
                pulled.append(1)
                yield b"x" * (1024 * 1024)

    def routes(r: httpx.Request) -> Any:
        if r.url.params.get("alt") == "media":
            return httpx.Response(200, stream=Endless(), headers={"content-type": "text/plain"})
        return {"name": "Big", "mimeType": "text/plain"}

    client, _ = google(routes, "drive")
    out = client.call_tool("read_file_content", {"fileId": "big"})
    assert out["isError"] and "larger than 1 MB" in out["content"][0]["text"]
    assert len(pulled) <= 3


def test_drive_download_still_reports_the_http_error() -> None:
    def routes(r: httpx.Request) -> Any:
        if r.url.params.get("alt") == "media":
            return httpx.Response(404, json={"error": {"message": "File not found"}})
        return {"name": "Gone", "mimeType": "text/plain"}

    client, _ = google(routes, "drive")
    out = client.call_tool("read_file_content", {"fileId": "gone"})
    assert out["isError"] and "HTTP 404: File not found" in out["content"][0]["text"]


def test_suggest_time_keeps_an_explicit_midnight_workday_start(monkeypatch: pytest.MonkeyPatch) -> None:
    # `or 9` read an explicit 0 as no answer and started the day at 9
    seen: dict[str, Any] = {}
    monkeypatch.setattr(google_rest, "free_slots",
                        lambda busy, start, end, minutes, day_start, day_end: seen.update(s=day_start, e=day_end) or [])

    def routes(r: httpx.Request) -> Any:
        return {"items": [{"id": "me@x"}]} if r.url.path.endswith("/calendarList") else {"calendars": {}}

    client, _ = google(routes, "calendar")
    args = {"startTime": "2026-10-05T00:00:00-07:00", "endTime": "2026-10-05T20:00:00-07:00", "durationMinutes": 30}
    client.call_tool("suggest_time", {**args, "workdayStartHour": 0})
    assert seen == {"s": 0, "e": 18}
    client.call_tool("suggest_time", args)
    assert seen == {"s": 9, "e": 18}                                   # unset still means 9 to 18


def test_a_google_error_is_a_tool_error_not_an_exception() -> None:
    client, _ = google(lambda r: httpx.Response(403, json={"error": {"message": "Gmail API has not been used"}}),
                       "gmail")
    out = client.call_tool("list_labels", {})
    assert out["isError"] and "HTTP 403: Gmail API has not been used" in out["content"][0]["text"]
    assert client.call_tool("send_email", {})["isError"]
    signed_out = google_rest.GoogleRestClient("drive", lambda: None)
    assert "not signed in" in signed_out.call_tool("list_recent_files", {})["content"][0]["text"]


def test_the_rest_tools_carry_the_mcp_servers_names() -> None:
    assert [t["name"] for t in google_rest.TOOLS["calendar"]][:2] == ["list_calendars", "list_events"]
    for app, tools in google_rest.TOOLS.items():
        for t in tools:
            assert hasattr(google_rest.GoogleRestClient, f"_{app}_{t['name']}"), (app, t["name"])
            assert apps_mcp.tool_reads(t) is t["annotations"]["readOnlyHint"], (app, t["name"])


def test_times_without_an_offset_get_the_macs() -> None:
    from datetime import datetime

    local = datetime(2026, 10, 5, 9, 0).astimezone().isoformat()
    assert google_rest.rfc3339("2026-10-05T09:00") == local
    assert google_rest.rfc3339("2026-10-05").startswith("2026-10-05T00:00:00")
    assert google_rest.rfc3339("2026-10-05T09:00:00Z") == "2026-10-05T09:00:00+00:00"
    assert google_rest.rfc3339("next friday") == "next friday"            # Google refuses it, said as an error


def test_free_slots_skip_busy_time_and_keep_to_working_hours() -> None:
    from datetime import datetime, timedelta, timezone

    tz = timezone(timedelta(hours=-7))
    day = datetime(2026, 10, 5, 0, 0, tzinfo=tz)
    busy = [(day.replace(hour=10), day.replace(hour=11)), (day.replace(hour=11, minute=15), day.replace(hour=17))]
    slots = google_rest.free_slots(busy, day, day + timedelta(days=1), 30)
    assert slots == [{"start": day.replace(hour=9).isoformat(), "end": day.replace(hour=10).isoformat()},
                     {"start": day.replace(hour=17).isoformat(), "end": day.replace(hour=18).isoformat()}]


def test_suggest_time_reads_freebusy_over_the_owners_calendars() -> None:
    def routes(r: httpx.Request) -> Any:
        if r.url.path.endswith("/calendarList"):
            return {"items": [{"id": "me@x", "accessRole": "owner"},
                              {"id": "en.usa#holiday@group.v.calendar.google.com", "accessRole": "reader"},
                              {"id": "class@cs.example.edu", "accessRole": "reader"}]}
        body = json.loads(r.content)
        # a subscribed calendar is busy time; a holiday calendar is not
        assert [i["id"] for i in body["items"]] == ["me@x", "class@cs.example.edu"]
        assert body["timeMin"].endswith("-07:00")
        return {"calendars": {"me@x": {"busy": [{"start": "2026-10-05T16:00:00Z", "end": "2026-10-06T00:00:00Z"}]}}}

    client, _ = google(routes, "calendar")
    out = client.call_tool("suggest_time", {"startTime": "2026-10-05T08:00:00-07:00",
                                            "endTime": "2026-10-05T20:00:00-07:00", "durationMinutes": 45})
    # busy 9:00-17:00 local, so the one free stretch inside 9-18 is 17:00-18:00
    assert out["structuredContent"] == {"free": [{"start": "2026-10-05T17:00:00-07:00",
                                                  "end": "2026-10-05T18:00:00-07:00"}], "calendarsChecked": 2}


def test_list_events_reads_every_calendar_the_owner_sees_in_one_call() -> None:
    """Review, 2026-10-05: list_events read the primary calendar only, so "my next class" (on a subscribed class
    calendar) came back wrong or took list_calendars plus a call per calendar, one round each; the rundown ran out
    of rounds. With no calendarId it now reads them all (holidays aside), merged soonest first, each tagged."""
    paths: list[str] = []

    def routes(r: httpx.Request) -> Any:
        paths.append(r.url.path)
        if r.url.path.endswith("/calendarList"):
            return {"items": [{"id": "me@x", "summary": "Me", "primary": True},
                              {"id": "en.usa#holiday@group.v.calendar.google.com", "summary": "Holidays"},
                              {"id": "class@cs.example.edu", "summary": "CS 101"}]}
        assert r.url.params["singleEvents"] == "true" and r.url.params["orderBy"] == "startTime"
        if "class%40cs.example.edu" in str(r.url) or "class@cs.example.edu" in r.url.path:
            return {"timeZone": "America/Los_Angeles", "items": [
                {"id": "c1", "summary": "Lecture", "start": {"dateTime": "2026-10-05T10:00:00-07:00"}}]}
        return {"timeZone": "America/Los_Angeles", "items": [
            {"id": "m1", "summary": "Dentist", "start": {"dateTime": "2026-10-05T15:00:00-07:00"}},
            {"id": "m0", "summary": "Trip", "start": {"date": "2026-10-05"}}]}

    client, _ = google(routes, "calendar")
    out = client.call_tool("list_events", {"startTime": "2026-10-05T08:00:00-07:00"})["structuredContent"]
    assert [e["summary"] for e in out["events"]] == ["Trip", "Lecture", "Dentist"]
    assert [e["calendar"] for e in out["events"]] == ["Me", "CS 101", "Me"]
    assert out["calendarsRead"] == 2 and not any("holiday" in p for p in paths)
    # one calendar named: that one only, as before
    paths.clear()
    one = client.call_tool("list_events", {"calendarId": "primary"})["structuredContent"]
    assert [p.rsplit("/", 2)[-2] for p in paths] == ["primary"] and len(one["events"]) == 2


def test_list_events_across_calendars_keeps_what_it_read_when_one_calendar_fails() -> None:
    def routes(r: httpx.Request) -> Any:
        if r.url.path.endswith("/calendarList"):
            return {"items": [{"id": "me@x", "summary": "Me", "primary": True},
                              {"id": "team@x", "summary": "Team"}]}
        if "team" in r.url.path:
            return httpx.Response(403, json={"error": {"message": "no access"}})
        return {"items": [{"id": "m1", "summary": "Dentist", "start": {"dateTime": "2026-10-05T15:00:00-07:00"}}]}

    client, _ = google(routes, "calendar")
    out = client.call_tool("list_events", {})
    assert out["isError"] is False
    got = out["structuredContent"]
    assert [e["summary"] for e in got["events"]] == ["Dentist"] and got["unreadable"] == ["Team"]


def test_responding_keeps_every_guest_and_changes_only_the_owner() -> None:
    sent: list[Any] = []

    def routes(r: httpx.Request) -> Any:
        if r.method == "GET":
            return {"attendees": [{"email": "ana@x", "responseStatus": "accepted"},
                                  {"email": "me@x", "self": True, "responseStatus": "needsAction"}]}
        sent.append((dict(r.url.params), json.loads(r.content)))
        return {"id": "e1"}

    client, _ = google(routes, "calendar")
    assert client.call_tool("respond_to_event", {"eventId": "e1", "responseStatus": "declined"})["isError"] is False
    params, body = sent[0]
    assert params["sendUpdates"] == "all"
    assert body == {"attendees": [{"email": "ana@x", "responseStatus": "accepted"},
                                  {"email": "me@x", "self": True, "responseStatus": "declined"}]}
    alone, _ = google(lambda r: {"attendees": []}, "calendar")
    assert "not a guest" in alone.call_tool("respond_to_event", {"eventId": "e", "responseStatus": "accepted"})[
        "content"][0]["text"]


def test_guests_are_emailed_only_when_there_are_guests_or_it_is_asked() -> None:
    params: list[dict[str, str]] = []

    def routes(r: httpx.Request) -> Any:
        if r.method == "GET":
            return {"attendees": [{"email": "ana@x"}]} if "withguests" in r.url.path else {}
        params.append(dict(r.url.params))
        return {}

    client, _ = google(routes, "calendar")
    client.call_tool("delete_event", {"eventId": "withguests"})
    client.call_tool("delete_event", {"eventId": "alone"})
    client.call_tool("delete_event", {"eventId": "withguests", "notifyGuests": False})
    client.call_tool("quick_add_event", {"text": "Lunch Friday noon"})
    assert [p["sendUpdates"] for p in params] == ["all", "none", "none", "none"]
    assert params[-1]["text"] == "Lunch Friday noon"


def test_update_adds_guests_to_the_ones_already_there() -> None:
    sent: list[Any] = []

    def routes(r: httpx.Request) -> Any:
        if r.method == "GET":
            return {"attendees": [{"email": "ana@x", "responseStatus": "accepted"}]}
        sent.append(json.loads(r.content))
        return {}

    client, _ = google(routes, "calendar")
    client.call_tool("update_event", {"eventId": "e", "addAttendeeEmails": ["bo@x", "ana@x"], "summary": "Sync"})
    assert sent[0] == {"summary": "Sync", "attendees": [{"email": "ana@x", "responseStatus": "accepted"},
                                                         {"email": "bo@x"}]}


def test_a_recurring_event_carries_its_zone() -> None:
    client, _ = google(lambda r: {}, "calendar")
    body = client._event_body({"startTime": "2026-10-05T09:00:00-07:00", "endTime": "2026-10-05T09:30:00-07:00",
                               "recurrence": ["RRULE:FREQ=WEEKLY;BYDAY=MO"]})
    assert body["recurrence"] == ["RRULE:FREQ=WEEKLY;BYDAY=MO"] and body["start"]["timeZone"]


def test_attachments_are_listed_and_a_text_one_is_read() -> None:
    payload = {"mimeType": "multipart/mixed", "parts": [
        {"mimeType": "text/plain", "body": {"data": b64("see attached")}},
        {"mimeType": "text/plain", "filename": "notes.txt", "body": {"attachmentId": "att1", "size": 5}}]}
    assert google_rest.mail_text(payload) == "see attached"                  # the .txt is not the body
    assert google_rest.attachments(payload) == [{"filename": "notes.txt", "mimeType": "text/plain", "size": 5,
                                                  "attachmentId": "att1"}]

    def routes(r: httpx.Request) -> Any:
        if r.url.path.endswith("/attachments/att1"):
            return {"data": b64("hello")}
        return {"payload": payload}

    client, _ = google(routes, "gmail")
    out = client.call_tool("get_attachment", {"messageId": "m1", "attachmentId": "att1"})["structuredContent"]
    assert out == {"filename": "notes.txt", "mimeType": "text/plain", "text": "hello", "clipped": False}


def test_a_pdf_is_read_with_pdfkit_where_it_exists() -> None:
    pytest.importorskip("Quartz")
    # A one-page PDF with the word Hello, built by hand (PDF 1.4, Helvetica).
    stream = b"BT /F1 24 Tf 72 720 Td (Hello) Tj ET"
    objs = [b"<< /Type /Catalog /Pages 2 0 R >>", b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R "
            b"/Resources << /Font << /F1 5 0 R >> >> >>",
            b"<< /Length %d >>\nstream\n" % len(stream) + stream + b"\nendstream",
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]
    out, offsets = b"%PDF-1.4\n", []
    for i, o in enumerate(objs, 1):
        offsets.append(len(out))
        out += b"%d 0 obj\n" % i + o + b"\nendobj\n"
    xref = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % (len(objs) + 1)
    out += b"".join(b"%010d 00000 n \n" % off for off in offsets)
    out += b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF" % (len(objs) + 1, xref)
    assert "Hello" in (google_rest.as_text(out, "application/pdf") or "")


def test_the_client_uses_the_protocol_version_the_server_agreed() -> None:
    def handler(msg: dict[str, Any], req: httpx.Request) -> httpx.Response:
        if msg["method"] == "initialize":
            return httpx.Response(200, json={"jsonrpc": "2.0", "id": msg["id"],
                                             "result": {"protocolVersion": "2025-03-26"}})
        if "id" not in msg:
            return httpx.Response(202)
        assert req.headers["mcp-protocol-version"] == "2025-03-26"
        return httpx.Response(200, json={"jsonrpc": "2.0", "id": msg["id"], "result": {"tools": []}})

    http, _ = mcp_server(handler)
    assert McpClient("https://x/mcp", lambda: None, http=http).list_tools() == []


def test_github_asks_for_ci_beside_its_toolsets() -> None:
    headers = dict(apps_mcp.SERVERS["github"].headers)
    assert headers["X-MCP-Toolsets"] == "context,repos,issues,pull_requests,users"
    assert headers["X-MCP-Tools"] == "actions_list,actions_get,get_job_logs"     # notifications: classic token only
    assert "X-MCP-Readonly" not in headers                     # it would override the ask policy's writes


def test_maps_replies_keep_their_links(tmp_path: Path) -> None:
    b, _ = bridge(tmp_path)
    assert "Google Maps link" in b.instructions()
