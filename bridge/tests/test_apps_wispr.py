"""Wispr Flow's meetings as a direct app (apps_mcp.py): its words, its sign-in, its token and the tools it offers.

Every HTTP exchange goes through httpx.MockTransport, so the real request code runs and nothing leaves the Mac.
"""
from __future__ import annotations

import json
import stat
import threading
import time
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any

import httpx
import pytest

from eve_bridge import apps_mcp, toolset_router
from eve_bridge.apps_mcp import AppsBridge, AppsConfig

# The server's own tool list, as it answered on 2026-11-11 (names and hints; every one marked read-only).
WISPR_TOOLS = [{"name": n, "annotations": {"readOnlyHint": True}} for n in (
    "search_meetings", "get_meeting", "get_meeting_participants_enriched", "get_participant_contacts",
    "resolve_share_link", "search_scratchpad_notes", "get_scratchpad_note", "search_calendar_events",
    "resolve_calendar_link", "get_calendar_event", "get_meeting_by_calendar_id", "list_meeting_series",
    "list_upcoming_meetings", "get_upcoming_meeting", "list_folders", "list_folder_meetings")]


def cfg(tmp_path: Path) -> AppsConfig:
    return AppsConfig(enabled=True, config_dir=tmp_path, policy=apps_mcp.policy({}), environ={})


@pytest.mark.parametrize("text", [
    "what did we decide in the standup", "summarize my meeting with ana yesterday", "action items from the sync",
    "what did sam say about pricing", "find the transcript of the design review", "anything in my wispr scratchpad",
    "prep me for my next meeting", "my 1:1 notes",
])
def test_meeting_words_open_wispr(text: str) -> None:
    assert apps_mcp.names_app("wispr", text)
    assert "wispr" in toolset_router.keyword_toolsets(text)


@pytest.mark.parametrize("text", ["turn the lights blue", "remind me in 10 minutes", "play some music",
                                  "what's the weather"])
def test_other_requests_leave_wispr_closed(text: str) -> None:
    assert not apps_mcp.names_app("wispr", text)


def test_wispr_is_read_only_and_offers_only_its_reads(tmp_path: Path) -> None:
    class Fake:
        def __init__(self) -> None:
            self.calls: list[Any] = []

        def list_tools(self) -> list[dict[str, Any]]:
            return [dict(t) for t in WISPR_TOOLS]

        def call_tool(self, name: str, args: Any) -> dict[str, Any]:
            self.calls.append((name, dict(args)))
            return {"structuredContent": {"meetings": []}}

    fake = Fake()
    b = AppsBridge(cfg(tmp_path), client_factory=lambda s, tok: fake, tokens={"wispr": lambda: "w"})
    b.start()
    assert b.apps == ["wispr"] and b.policy["wispr"] == "read"
    names = {t["name"] for t in b.tools()}
    assert "wispr__search_meetings" in names and "wispr__get_meeting" in names
    assert "wispr__resolve_share_link" not in names          # "share" is a writing word: not offered under read
    assert len(names) == len(WISPR_TOOLS) - 1
    assert b.decide("wispr__search_meetings", {"query": "pricing"}).action == "run"
    assert b.execute("wispr__search_meetings", {"query": "pricing"})["ok"] is not False
    assert fake.calls == [("search_meetings", {"query": "pricing"})]
    assert "Wispr Flow meetings (wispr__…): read only" in b.instructions()


def test_the_token_refreshes_with_the_registered_client_and_keeps_a_rotated_refresh_token(tmp_path: Path) -> None:
    c = cfg(tmp_path)
    assert not apps_mcp.TokenSource("wispr", c).available
    apps_mcp.write_private_json(c.wispr_token_path, {"client_id": "cid", "refresh_token": "r1",
                                                     "access_token": "old", "expires_at": time.time() - 5})
    seen: list[dict[str, str]] = []

    def respond(req: httpx.Request) -> httpx.Response:
        assert str(req.url) == apps_mcp.WISPR_AUTH + "/oauth2/token"
        seen.append(dict(urllib.parse.parse_qsl(req.content.decode())))
        return httpx.Response(200, json={"access_token": "new", "expires_in": 3600, "refresh_token": "r2"})

    src = apps_mcp.TokenSource("wispr", c, http=httpx.Client(transport=httpx.MockTransport(respond)))
    assert src.available and src() == "new"
    assert seen == [{"grant_type": "refresh_token", "client_id": "cid", "refresh_token": "r1",
                     "resource": apps_mcp.WISPR_MCP_URL}]
    saved = json.loads(c.wispr_token_path.read_text())
    assert saved["refresh_token"] == "r2" and saved["access_token"] == "new"
    assert stat.S_IMODE(c.wispr_token_path.stat().st_mode) == 0o600
    assert src() == "new" and len(seen) == 1                 # still fresh: no second refresh


def test_a_refused_refresh_is_none_and_recorded(tmp_path: Path) -> None:
    c = cfg(tmp_path)
    apps_mcp.write_private_json(c.wispr_token_path, {"client_id": "cid", "refresh_token": "r1"})
    http = httpx.Client(transport=httpx.MockTransport(lambda r: httpx.Response(400, json={"error": "invalid_grant"})))
    try:
        assert apps_mcp.TokenSource("wispr", c, http=http)() is None
        assert "wispr" in apps_mcp.refusals
    finally:
        apps_mcp.refusals.pop("wispr", None)


def test_login_registers_signs_in_with_pkce_and_stores_the_tokens(tmp_path: Path) -> None:
    c = cfg(tmp_path)
    seen: dict[str, Any] = {}

    def respond(req: httpx.Request) -> httpx.Response:
        path = req.url.path
        if path == "/oauth2/register":
            body = json.loads(req.content)
            seen["register"] = body
            return httpx.Response(201, json={"client_id": "cid"})
        if path == "/oauth2/token":
            seen["token"] = dict(urllib.parse.parse_qsl(req.content.decode()))
            return httpx.Response(200, json={"access_token": "a", "refresh_token": "r", "expires_in": 3600,
                                             "scope": "openid offline_access"})
        return httpx.Response(404)

    def open_url(url: str) -> None:
        q = dict(urllib.parse.parse_qsl(urllib.parse.urlparse(url).query))
        seen["authorize"] = q
        # the browser comes back to the loopback redirect with a code and the same state
        back = q["redirect_uri"] + "?" + urllib.parse.urlencode({"code": "c0de", "state": q["state"]})
        threading.Thread(target=lambda: urllib.request.urlopen(back, timeout=10).read(), daemon=True).start()

    apps_mcp.wispr_login(c, open_url, say=lambda _s: None,
                         http=httpx.Client(transport=httpx.MockTransport(respond)))
    reg, auth, tok = seen["register"], seen["authorize"], seen["token"]
    assert reg["token_endpoint_auth_method"] == "none" and reg["redirect_uris"] == [auth["redirect_uri"]]
    assert auth["redirect_uri"].startswith("http://127.0.0.1:") and auth["code_challenge_method"] == "S256"
    assert auth["client_id"] == "cid" and auth["resource"] == apps_mcp.WISPR_MCP_URL
    assert tok["code"] == "c0de" and tok["client_id"] == "cid" and tok["code_verifier"]
    saved = json.loads(c.wispr_token_path.read_text())
    assert saved["client_id"] == "cid" and saved["refresh_token"] == "r"
    assert stat.S_IMODE(c.wispr_token_path.stat().st_mode) == 0o600
