"""eve lite's apps (composio_connect.py, WP-6): ConnectTokenSource and ConnectBridge against a fake Composio Connect.

The fake serves spike S1's captures as they are (tests/fixtures/composio_connect): the seven meta tool objects from
tools/list, and the two 401 answers /mcp gives a missing or an unknown bearer. Its token endpoint takes the refresh form
S1 verified. Every exchange goes through httpx.MockTransport, so the real McpClient and httpx code runs and nothing
leaves the Mac. Tokens are canaries that do not look like tokens (AGENTS rule 2).
"""

from __future__ import annotations

import hashlib
import itertools
import json
import logging
import os
import stat
import threading
import time
from pathlib import Path
from typing import Any, Callable, Optional
from urllib.parse import parse_qs, urlsplit

import httpx
import pytest

from eve_bridge import apps_mcp, composio_connect, composio_tools, profile
from eve_bridge.composio_connect import (
    MANAGE_CONNECTIONS,
    META_TOOLS,
    SEARCH_TOOLS,
    ConnectBridge,
    ConnectTokenSource,
)

FIXTURES = Path(__file__).parent / "fixtures" / "composio_connect"
MCP_URL = "https://connect.composio.dev/mcp"
TOKEN_URL = "https://login.composio.dev/oauth2/token"
CANARY = "CANARY-NOT-A-KEY-7f3c"
A1, A2, A3, A4 = (f"{CANARY}-access-{n}" for n in (1, 2, 3, 4))
R1, R2, R3, R4 = (f"{CANARY}-refresh-{n}" for n in (1, 2, 3, 4))


def fixture(name: str) -> Any:
    return json.loads((FIXTURES / name).read_text(encoding="utf-8"))


META = {name: fixture(f"{name}.json") for name in META_TOOLS}
MCP_401 = fixture("mcp_401.json")["cases"]
TOKEN_PROBE = fixture("token_probe.json")["cases"]
CLIENT_ID = fixture("registration.json")["response"]["client_id"]
LINK = "https://connect.composio.dev/link/ln_0000test"


def sign_in(path: Path, *, access: str = A1, refresh: str = R1, expires_in: float = 3600.0,
            now: Optional[float] = None, **extra: Any) -> Path:
    """composio-connect.json as onboarding.ComposioConnectLogin writes it: its contract keys, 0600."""
    apps_mcp.write_private_json(path, {
        "version": 1, "issuer": "https://connect.composio.dev", "client_id": CLIENT_ID,
        "redirect_uris": ["http://localhost:48780/onboard/composio/callback",
                          "http://127.0.0.1:48780/onboard/composio/callback"],
        "access_token": access, "refresh_token": refresh,
        "expires_at": (time.time() if now is None else now) + expires_in,
        "scope": "openid profile email offline_access", **extra})
    return path


def rpc(msg: dict[str, Any], result: Any, headers: Optional[dict[str, str]] = None) -> httpx.Response:
    return httpx.Response(200, json={"jsonrpc": "2.0", "id": msg.get("id"), "result": result}, headers=headers)


def text_result(obj: Any) -> dict[str, Any]:
    """An MCP tools/call result whose one text part is ``obj`` as JSON. S1 ran no tools/call, so this shape (Composio's
    own object as the text) is an assumption; the bridge reads structuredContent as well, and plain text as text."""
    return {"content": [{"type": "text", "text": json.dumps(obj)}]}


def unauthorized(case: int) -> httpx.Response:
    """/mcp's 401 as S1 captured it: 0 for no bearer, 1 for a bearer that is not a token Composio knows."""
    c = MCP_401[case]
    return httpx.Response(c["status"], json=c["body"], headers={"WWW-Authenticate": c["www_authenticate"]})


class Composio:
    """A fake Composio Connect: /mcp and the token endpoint, every request kept."""

    def __init__(self, tools: Optional[list[Any]] = None) -> None:
        self.tools = [dict(t) for t in META.values()] if tools is None else tools
        self.live: set[str] = {A1}                       # the access tokens /mcp takes
        self.grants: dict[str, tuple[str, str]] = {R1: (A2, R2)}   # refresh -> (access, refresh), each used once
        self.take_new = True                             # a refreshed access token is taken at /mcp
        self.answers: dict[str, Callable[[dict[str, Any]], Any]] = {}   # tool name -> args -> result
        self.hook: Optional[Callable[[dict[str, Any]], Optional[httpx.Response]]] = None
        self.token_reply: Optional[httpx.Response] = None
        self.requests: list[httpx.Request] = []
        self._lock = threading.Lock()

    @property
    def transport(self) -> httpx.MockTransport:
        return httpx.MockTransport(self.handle)

    def handle(self, request: httpx.Request) -> httpx.Response:
        with self._lock:
            self.requests.append(request)
        if str(request.url) == TOKEN_URL:
            return self.token(request)
        assert str(request.url) == MCP_URL, request.url
        msg = json.loads(request.content)
        auth = request.headers.get("authorization", "")
        if not auth.startswith("Bearer "):
            return unauthorized(0)
        if auth.removeprefix("Bearer ") not in self.live:
            return unauthorized(1)
        if self.hook is not None:
            hooked = self.hook(msg)
            if hooked is not None:
                return hooked
        method = msg.get("method")
        if method == "initialize":
            return rpc(msg, {"protocolVersion": "2025-06-18", "capabilities": {"tools": {"listChanged": True}},
                             "serverInfo": {"name": "mcp-typescript server on vercel", "version": "0.1.0"}},
                       headers={"Mcp-Session-Id": "session-1"})
        if "id" not in msg:
            return httpx.Response(202)
        if method == "tools/list":
            return rpc(msg, {"tools": self.tools})
        if method == "tools/call":
            params = msg["params"]
            answer = self.answers.get(params["name"], lambda args: text_result(
                {"data": {"echo": args}, "error": None, "successful": True}))(params["arguments"])
            return answer if isinstance(answer, httpx.Response) else rpc(msg, answer)
        return httpx.Response(200, json={"jsonrpc": "2.0", "id": msg["id"],
                                         "error": {"code": -32601, "message": "no such method"}})

    def token(self, request: httpx.Request) -> httpx.Response:
        if self.token_reply is not None:
            return self.token_reply
        form = parse_qs(request.content.decode(), keep_blank_values=True)
        grant = self.grants.pop(form["refresh_token"][0], None)
        if grant is None:
            return httpx.Response(400, json=TOKEN_PROBE[1]["answer"])          # invalid_grant, as S1 saw it
        access, refresh = grant
        if self.take_new:
            self.live.add(access)
        return httpx.Response(200, json={"access_token": access, "refresh_token": refresh, "token_type": "Bearer",
                                         "expires_in": 3600})

    # what reached the fake
    def calls(self, method: str) -> list[dict[str, Any]]:
        return [json.loads(r.content) for r in self.requests
                if str(r.url) == MCP_URL and json.loads(r.content).get("method") == method]

    def bearers(self, method: str) -> list[str]:
        return [r.headers.get("authorization", "").removeprefix("Bearer ") for r in self.requests
                if str(r.url) == MCP_URL and json.loads(r.content).get("method") == method]

    def token_forms(self) -> list[dict[str, list[str]]]:
        return [parse_qs(r.content.decode(), keep_blank_values=True) for r in self.requests
                if str(r.url) == TOKEN_URL]


@pytest.fixture
def fake() -> Composio:
    return Composio()


@pytest.fixture
def bridges() -> Any:
    """Each bridge a test makes is closed after it, so no start thread outlives its test."""
    made: list[ConnectBridge] = []
    yield made
    for bridge in made:
        bridge.close()


def connect(tmp_path: Path, fake: Composio, bridges: list[ConnectBridge], *, signed_in: bool = True,
            start: bool = True, now: Optional[list[float]] = None, **kwargs: Any) -> ConnectBridge:
    """A bridge over the fake, signed in unless told otherwise. ``now`` is the token source's wall clock (and the
    sign-in's), for tests that move time; ``kwargs`` go to the bridge."""
    def wall() -> float:
        return time.time() if now is None else now[0]

    path = tmp_path / "composio-connect.json"
    if signed_in and not path.exists():
        sign_in(path, now=wall())
    bridge = ConnectBridge(ConnectTokenSource(path, transport=fake.transport, clock=wall), transport=fake.transport,
                           **kwargs)
    bridges.append(bridge)
    if start:
        assert bridge.try_start()
    return bridge


def wait_until(check: Callable[[], bool], secs: float = 5.0) -> bool:
    deadline = time.monotonic() + secs
    while time.monotonic() < deadline:
        if check():
            return True
        time.sleep(0.01)
    return check()


def mode(path: Path) -> int:
    return stat.S_IMODE(os.stat(path).st_mode)


# ---- the fixtures and the tools as the brain gets them ---------------------------------------------------------

def test_the_fixtures_are_the_ones_spike_s1_captured() -> None:
    files = fixture("provenance.json")["files"]
    assert sorted(files) == sorted(f"{name}.json" for name in META_TOOLS)
    for name, digest in files.items():
        assert hashlib.sha256((FIXTURES / name).read_bytes()).hexdigest() == digest, name
    assert [META[name]["name"] for name in META_TOOLS] == list(META_TOOLS)
    assert [c["status"] for c in MCP_401] == [401, 401]


def test_the_seven_meta_tools_are_offered_under_composios_own_names() -> None:
    tools = composio_connect.offered(list(META.values()))
    assert [t["name"] for t in tools] == list(META_TOOLS)
    for tool in tools:
        raw = META[tool["name"]]
        assert tool["type"] == "function" and tool["strict"] is False
        assert tool["parameters"] == apps_mcp.slim_schema(raw["inputSchema"])
        assert "$schema" not in tool["parameters"]
        assert tool["parameters"]["required"] == raw["inputSchema"]["required"]
        assert profile.lite_tool_allowed(tool["name"])                     # eve lite's filter keeps every one
    # the property descriptions are clipped as the direct apps' are; the arguments themselves are all there
    multi = {t["name"]: t for t in tools}["COMPOSIO_MULTI_EXECUTE_TOOL"]["parameters"]
    assert set(multi["properties"]) == set(META["COMPOSIO_MULTI_EXECUTE_TOOL"]["inputSchema"]["properties"])
    assert all(len(p.get("description", "")) <= apps_mcp.PROP_CLIP for p in multi["properties"].values())


def test_the_descriptions_keep_composios_protocol_and_leave_out_the_catalog_line() -> None:
    tools = {t["name"]: t for t in composio_connect.offered(list(META.values()))}
    raw = META[SEARCH_TOOLS]["description"]
    # control: the captured description names the web tools eve never runs, in its catalog line
    assert "Tool Server Info:" in raw and "Browser tool" in raw and "Web Search" in raw
    for tool in tools.values():
        text = tool["description"]
        assert "Tool Server Info" not in text and "Browser tool" not in text and "Web Search" not in text
        assert text == text.strip() and "\n\n\n" not in text and len(text) <= composio_connect.DESC_MAX
    # what the model needs from them is all there, not clipped at apps_mcp.DESC_CLIP
    search = tools[SEARCH_TOOLS]["description"]
    assert "SESSION: ALWAYS set this parameter" in search and "COMPOSIO_GET_TOOL_SCHEMAS first" in search
    assert len(search) > 3 * apps_mcp.DESC_CLIP
    assert "redirect_url as a FORMATTED MARKDOWN LINK" in tools[MANAGE_CONNECTIONS]["description"]
    assert "sync_response_to_workbench" in tools["COMPOSIO_MULTI_EXECUTE_TOOL"]["description"]
    # every other line of Composio's text is there as it was
    kept = [line.strip() for line in raw.splitlines() if line.strip() and "Tool Server Info:" not in line]
    assert [line.strip() for line in search.splitlines() if line.strip()] == kept


def test_a_description_is_clipped_past_its_limit_and_falls_back_to_the_title() -> None:
    long = composio_connect.describe({"name": "COMPOSIO_X", "description": "word " * 5000})
    assert len(long) == composio_connect.DESC_MAX and long.endswith("…")
    assert composio_connect.describe({"name": "COMPOSIO_X", "title": " Search ", "description": "\n  "}) == "Search"
    assert composio_connect.describe({"name": "COMPOSIO_X", "description": 7}) == "COMPOSIO_X"


def test_tools_beyond_the_seven_are_left_out_and_the_first_of_a_name_counts() -> None:
    listed: list[Any] = [
        {"name": "COMPOSIO_SEARCH_WEB", "description": "a web search", "inputSchema": {"type": "object"}},
        {"name": "GMAIL_SEND_EMAIL", "description": "sends", "inputSchema": {"type": "object"}},
        "not a tool", {"name": ["not", "a", "name"]}, {"description": "no name"},
        dict(META[MANAGE_CONNECTIONS]),
        {**META[MANAGE_CONNECTIONS], "description": "a second copy"},
        {"name": SEARCH_TOOLS, "inputSchema": "not a schema"},
        {"name": "COMPOSIO_REMOTE_BASH_TOOL", "inputSchema": {"type": "object", "properties": ["x"]}},
    ]
    tools = composio_connect.offered(listed)
    assert [t["name"] for t in tools] == [SEARCH_TOOLS, MANAGE_CONNECTIONS, "COMPOSIO_REMOTE_BASH_TOOL"]
    assert tools[1]["description"] == composio_connect.describe(META[MANAGE_CONNECTIONS])
    # a schema the model's API would refuse is replaced by an empty one, never sent as it is
    assert tools[0]["parameters"] == tools[2]["parameters"] == {"type": "object", "properties": {}}


@pytest.mark.parametrize("url", [
    LINK, "https://composio.dev/link/x", "https://app.composio.dev/a?b=c#d", "https://connect.composio.dev:443/link/x",
    "HTTPS://Connect.Composio.Dev/link/x",
])
def test_a_connect_link_on_composio_dev_is_accepted(url: str) -> None:
    assert composio_connect.link_ok(url)


@pytest.mark.parametrize("url", [
    "http://connect.composio.dev/link/x",                 # not https
    "https://composio.dev.example.com/link/x",            # another site's subdomain
    "https://evilcomposio.dev/link/x",                    # a name that ends like it
    "https://connect.composio.dev@example.com/x",         # a user part: the host is example.com
    "https://user:pw@connect.composio.dev/x",             # a user part on the right host
    "https://example.com\\@connect.composio.dev/x",       # a browser reads example.com
    "https://example.com#@connect.composio.dev",
    "https://connect.composio.dev:8443/link/x",           # another port
    "https://connect.composio.dev./link/x",               # a trailing dot
    "https://connect.cоmposio.dev/link/x",           # a Cyrillic o
    "https://connect.composio.dev%2eexample.com/x",
    "https://example.com%2f.composio.dev/x", "https://a..composio.dev/x", "https://x_y.composio.dev/x",   # the host
    "https://connect.composio.dev/link/а", "https://connect.composio.dev/‮link",   # what the page shows
    "https://[::1]/x", "https://connect.composio.dev/li nk", "https://connect.composio.dev/\nx",
    "//connect.composio.dev/x", "javascript:alert(1)//connect.composio.dev", "data:text/html,composio.dev",
    "", "https://", None, 7, ["https://connect.composio.dev/x"], "https://connect.composio.dev/" + "x" * 5000,
    # what Python cannot split: refused, never raised (urlsplit raises ValueError for each)
    "https://[x/y", "https://connect.composio.dev]/x", "https://[connect.composio.dev]/x", "https://[1.2.3.4]/x",
    "https://[vz]/x", "https://a]b[c.composio.dev/x",
])
def test_a_connect_link_anywhere_else_is_refused(url: Any) -> None:
    assert composio_connect.link_ok(url) is False


def test_link_ok_answers_every_url_python_cannot_split() -> None:
    """Every short URL over the characters that trip urlsplit gets a plain answer: link_ok is public, and its
    callers hand it whatever Composio sent."""
    urls = [base + "".join(tail) for base in ("https://", "https://connect.composio.dev", "")
            for size in range(5) for tail in itertools.product("[]:@/.v1a", repeat=size)]
    unsplittable = 0
    for url in urls:
        try:
            urlsplit(url)
        except ValueError:
            unsplittable += 1
        assert isinstance(composio_connect.link_ok(url), bool), url
    assert unsplittable > 1000                            # control: these inputs reach urlsplit's ValueError
    assert composio_connect.link_ok("https://connect.composio.dev/link/ln_1?a=[1]")   # a bracket past the host


@pytest.mark.parametrize(("toolkit", "slug"), [
    ("gmail", "gmail"), (" GoogleCalendar ", "googlecalendar"), ("google_maps", "google_maps"),
    ("../gmail", None), ("gmail;rm", None), ("", None), ("_x", None), ("x" * 65, None), (None, None), (5, None),
])
def test_a_toolkit_slug_is_lowercase_letters_digits_and_underscores(toolkit: Any, slug: Optional[str]) -> None:
    assert composio_connect.toolkit_slug(toolkit) == slug


# ---- the sign-in ---------------------------------------------------------------------------------------------

def test_a_live_token_is_handed_out_with_no_request(tmp_path: Path, fake: Composio) -> None:
    src = ConnectTokenSource(sign_in(tmp_path / "c.json"), transport=fake.transport)
    assert src() == A1 and src() == A1 and fake.requests == []
    assert not src.needs_sign_in and A1 not in repr(src) and R1 not in repr(src)


def test_a_token_near_expiry_is_refreshed_with_the_form_s1_verified_and_saved_0600(tmp_path: Path,
                                                                                 fake: Composio) -> None:
    path = sign_in(tmp_path / "c.json", expires_in=30)
    before = json.loads(path.read_text())
    src = ConnectTokenSource(path, transport=fake.transport)
    assert src() == A2
    [request] = fake.requests
    assert str(request.url) == TOKEN_URL and request.method == "POST"
    assert request.headers["content-type"] == "application/x-www-form-urlencoded"
    assert "authorization" not in request.headers                          # a public client: no secret, no Basic
    form = parse_qs(request.content.decode(), keep_blank_values=True)
    assert sorted(form) == sorted(TOKEN_PROBE[1]["request_keys"])
    assert form == {"grant_type": ["refresh_token"], "refresh_token": [R1], "client_id": [CLIENT_ID],
                    "resource": [MCP_URL]}
    saved = json.loads(path.read_text())
    assert saved == {**before, "access_token": A2, "refresh_token": R2, "expires_at": pytest.approx(time.time() + 3600,
                                                                                                   abs=30)}
    assert mode(path) == 0o600
    assert src() == A2 and len(fake.requests) == 1                         # the new token holds for an hour


def test_a_refresh_that_brings_no_new_refresh_token_keeps_the_old_one(tmp_path: Path, fake: Composio) -> None:
    path = sign_in(tmp_path / "c.json", expires_in=-5)
    fake.token_reply = httpx.Response(200, json={"access_token": A2, "expires_in": 120, "scope": "openid email"})
    assert ConnectTokenSource(path, transport=fake.transport)() == A2
    saved = json.loads(path.read_text())
    assert saved["refresh_token"] == R1 and saved["access_token"] == A2 and saved["scope"] == "openid email"
    assert saved["expires_at"] == pytest.approx(time.time() + 120, abs=30)


@pytest.mark.parametrize(("status", "body"), [
    (400, TOKEN_PROBE[1]["answer"]),                      # invalid_grant: the refresh token is gone
    (401, TOKEN_PROBE[3]["answer"]),                      # invalid_client: the registration is gone
    (401, {}),                                            # a 401 says the client is refused, error code or not
])
def test_a_refresh_composio_refuses_is_never_sent_again(tmp_path: Path, fake: Composio, caplog: Any,
                                                        status: int, body: dict[str, Any]) -> None:
    path = sign_in(tmp_path / "c.json", expires_in=-5)
    fake.token_reply = httpx.Response(status, json=body)
    src = ConnectTokenSource(path, transport=fake.transport)
    with caplog.at_level(logging.WARNING):
        assert src() is None and src() is None and src.refresh(A1) is None
    assert len(fake.token_forms()) == 1 and src.needs_sign_in
    assert f"HTTP {status}, {body.get('error', 'no error code')}" in caplog.text and CANARY not in caplog.text
    sign_in(path, access=A3, refresh=R3, expires_in=-5)                   # signed in again: a new refresh token
    fake.token_reply = None
    fake.grants[R3] = (A2, R2)
    assert src() == A2 and not src.needs_sign_in


def test_a_refresh_that_fails_for_a_reason_that_may_pass_keeps_a_live_token_and_waits(tmp_path: Path,
                                                                                    fake: Composio) -> None:
    now = [1_000_000.0]
    path = sign_in(tmp_path / "c.json", expires_in=30, now=now[0])
    fake.token_reply = httpx.Response(503, text="busy")
    src = ConnectTokenSource(path, transport=fake.transport, clock=lambda: now[0])
    assert src() == A1                                    # 30 s left: the token still goes
    assert src() == A1 and len(fake.token_forms()) == 1   # no second try this soon
    now[0] += composio_connect.REFRESH_RETRY_SECS + 1
    fake.token_reply = None
    assert src() == A2 and len(fake.token_forms()) == 2
    now[0] += 7200                                        # A2 lapsed; the token endpoint cannot be reached

    def unreachable(request: httpx.Request) -> httpx.Response:
        raise httpx.ConnectError("no route", request=request)

    offline = ConnectTokenSource(path, transport=httpx.MockTransport(unreachable), clock=lambda: now[0])
    assert offline() is None and not offline.needs_sign_in   # lapsed, but a refresh may still work later


def test_a_token_answer_nested_past_pythons_limit_is_a_failed_refresh(tmp_path: Path, fake: Composio) -> None:
    path = sign_in(tmp_path / "c.json", expires_in=30)
    fake.token_reply = httpx.Response(200, text="[" * 100_000 + "]" * 100_000,
                                      headers={"content-type": "application/json"})
    src = ConnectTokenSource(path, transport=fake.transport)
    assert src() == A1 and src() == A1                    # the live token still goes, and the refresh cools off
    assert len(fake.token_forms()) == 1


def test_no_sign_in_eve_can_use_is_no_token_and_no_request(tmp_path: Path, fake: Composio) -> None:
    missing = ConnectTokenSource(tmp_path / "none.json", transport=fake.transport)
    assert missing() is None and missing.refresh(A1) is None and missing.needs_sign_in
    for extra in ({"version": 2}, {"issuer": "https://login.composio.dev"}):
        path = sign_in(tmp_path / "c.json", **extra)
        src = ConnectTokenSource(path, transport=fake.transport)
        assert src() is None and src.needs_sign_in
    (tmp_path / "bad.json").write_text("{not json")
    assert ConnectTokenSource(tmp_path / "bad.json", transport=fake.transport)() is None
    path = sign_in(tmp_path / "nore.json", refresh="", expires_in=-5)
    assert ConnectTokenSource(path, transport=fake.transport)() is None
    assert fake.requests == []


def test_after_a_401_a_token_another_call_already_refreshed_is_used_with_no_request(tmp_path: Path,
                                                                                    fake: Composio) -> None:
    path = sign_in(tmp_path / "c.json", access=A2, refresh=R2)
    src = ConnectTokenSource(path, transport=fake.transport)
    assert src.refresh(A1) == A2 and fake.requests == []
    assert src.refresh(A2) is None and len(fake.token_forms()) == 1      # R2 has no grant in the fake: refused
    assert src() is None                                  # A2 was refused: it is never sent again


def test_after_a_401_the_files_other_token_goes_as_it_is_until_it_lapses(tmp_path: Path, fake: Composio) -> None:
    """The early refresh is for an ordinary call. After a 401, a token another call put in the file goes at once
    while it has not lapsed, with no second token pair minted beside the one that call just got."""
    now = [1_000_000.0]
    path = sign_in(tmp_path / "c.json", access=A2, refresh=R2, expires_in=30, now=now[0])
    fake.grants[R2] = (A3, R3)
    src = ConnectTokenSource(path, transport=fake.transport, clock=lambda: now[0])
    assert src.refresh(A1) == A2 and fake.requests == []                 # 30 s left: it goes as it is
    assert src() == A3 and len(fake.token_forms()) == 1                  # control: an ordinary call refreshes it


def test_a_hold_sends_nothing_for_a_while_then_tries_the_same_token(tmp_path: Path, fake: Composio) -> None:
    now = [1_000_000.0]
    path = sign_in(tmp_path / "c.json", access="", now=now[0])   # no access token yet: the first call refreshes
    src = ConnectTokenSource(path, transport=fake.transport, clock=lambda: now[0])
    src.hold(None)
    src.hold("")                                          # no token named: nothing to hold
    assert src() == A2 and len(fake.token_forms()) == 1
    src.hold(A1)                                          # not the token the file holds: nothing changes
    assert src() == A2
    src.hold(A2)
    fake.grants[R2] = (A3, R3)
    assert src() is None and src.refresh(A2) is None      # held: no token goes out, and no pair is minted
    assert not src.needs_sign_in and len(fake.token_forms()) == 1        # a hold never asks for a new sign-in
    now[0] += composio_connect.REFUSED_RETRY_SECS - 1
    assert src() is None and src.refresh(A2) is None
    now[0] += 1
    assert src() == A2 and len(fake.token_forms()) == 1                  # over: the same token, with no request
    now[0] -= 100
    assert src() == A2                                    # and over for good: a clock set back does not revive it
    assert src.refresh(A2) == A3 and len(fake.token_forms()) == 2        # after a 401 to it, one refresh
    src.hold(A3)
    sign_in(path, access=A4, refresh=R4, now=now[0])      # signed in again: that sign-in is not held
    assert src() == A4 and not src.needs_sign_in


def test_a_clock_set_back_does_not_stretch_a_cool_off(tmp_path: Path, fake: Composio) -> None:
    now = [1_000_000.0]
    path = sign_in(tmp_path / "c.json", expires_in=30, now=now[0])
    fake.token_reply = httpx.Response(503, text="busy")
    src = ConnectTokenSource(path, transport=fake.transport, clock=lambda: now[0])
    assert src() == A1 and src.refresh(A1) is None and len(fake.token_forms()) == 1   # a refresh cooling off
    now[0] -= 3600                                        # the wall clock set back an hour
    fake.token_reply = None
    assert src.refresh(A1) == A2 and len(fake.token_forms()) == 2        # over, not an hour longer
    src.hold(A2)
    assert src() is None
    now[0] -= 3600
    assert src() == A2                                    # a hold is not stretched either


def test_a_new_sign_in_written_during_a_refresh_is_kept(tmp_path: Path, fake: Composio) -> None:
    path = sign_in(tmp_path / "c.json", expires_in=-5)

    def meanwhile(request: httpx.Request) -> httpx.Response:
        sign_in(path, access=A3, refresh=R3)                              # the setup page signs in again
        return httpx.Response(200, json={"access_token": A2, "refresh_token": R2, "expires_in": 3600})

    src = ConnectTokenSource(path, transport=httpx.MockTransport(meanwhile))
    assert src() == A2
    saved = json.loads(path.read_text())
    assert saved["access_token"] == A3 and saved["refresh_token"] == R3
    assert src() == A3


def test_a_refresh_that_cannot_be_saved_is_held_until_a_restart(tmp_path: Path, fake: Composio,
                                                                 monkeypatch: pytest.MonkeyPatch) -> None:
    path = sign_in(tmp_path / "c.json", expires_in=-5)

    def disk_full(*_: Any, **__: Any) -> None:
        raise OSError(28, "No space left on device")

    monkeypatch.setattr(composio_connect, "write_private_json", disk_full)
    src = ConnectTokenSource(path, transport=fake.transport)
    assert src() == A2 and src() == A2 and len(fake.token_forms()) == 1
    assert json.loads(path.read_text())["refresh_token"] == R1           # not saved, and not lost: held in memory
    sign_in(path, access=A3, refresh=R3)                                  # a new sign-in replaces what was held
    assert src() == A3


# ---- the bridge over MCP -------------------------------------------------------------------------------------

def test_the_bridge_lists_the_seven_meta_tools_over_mcp(tmp_path: Path, fake: Composio,
                                                        bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges, start=False)
    assert not bridge.started and bridge.tools() == [] and bridge.names == frozenset()
    assert bridge.try_start() and bridge.started and bridge.try_start()
    assert [json.loads(r.content)["method"] for r in fake.requests] == [
        "initialize", "notifications/initialized", "tools/list"]
    assert {r.headers["authorization"] for r in fake.requests} == {f"Bearer {A1}"}
    assert fake.requests[2].headers["mcp-session-id"] == "session-1"
    assert fake.requests[0].headers["mcp-protocol-version"] == apps_mcp.PROTOCOL_VERSION
    assert bridge.names == frozenset(META_TOOLS)
    assert bridge.tools() == composio_connect.offered(list(META.values()))
    assert bridge.accounts_note() == ""
    copy = bridge.tools()
    copy[0]["name"] = "changed"
    assert bridge.tools()[0]["name"] == SEARCH_TOOLS                      # each caller gets its own list


def test_a_server_without_the_meta_tools_does_not_start(tmp_path: Path, bridges: list[ConnectBridge]) -> None:
    fake = Composio(tools=[{"name": "GMAIL_SEND_EMAIL", "inputSchema": {"type": "object"}}])
    bridge = connect(tmp_path, fake, bridges, start=False)
    assert not bridge.try_start() and not bridge.started and bridge.tools() == []
    assert bridge.execute(SEARCH_TOOLS, {}) == {"ok": False, "reason": composio_connect.NOT_UP}


def test_signed_out_the_bridge_makes_no_request(tmp_path: Path, fake: Composio, bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges, signed_in=False, start=False)
    assert not bridge.try_start() and bridge.needs_sign_in
    assert bridge.connect_link("gmail") is None
    assert bridge.connections() == dict.fromkeys(composio_connect.APPS, False)
    assert fake.requests == []


def test_execute_runs_a_meta_tool_and_answers_with_composios_own_object(tmp_path: Path, fake: Composio,
                                                                        bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)
    answer = {"data": {"results": [{"response": {"data": {"messages": [{"subject": "Lunch"}]}}}]}, "error": None,
              "successful": True, "log_id": "log_1"}
    fake.answers["COMPOSIO_MULTI_EXECUTE_TOOL"] = lambda args: text_result(answer)
    args = {"tools": [{"tool_slug": "GMAIL_FETCH_EMAILS", "arguments": {"query": "from:sam"}}],
            "sync_response_to_workbench": False, "session_id": "s-1"}
    out = bridge.execute("COMPOSIO_MULTI_EXECUTE_TOOL", args)
    assert out == {"ok": True, **answer}
    [call] = fake.calls("tools/call")
    assert call["params"] == {"name": "COMPOSIO_MULTI_EXECUTE_TOOL", "arguments": args}


def test_execute_reads_structured_content_from_an_event_stream(tmp_path: Path, fake: Composio,
                                                               bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)

    def stream(msg: dict[str, Any]) -> Optional[httpx.Response]:
        if msg.get("method") != "tools/call":
            return None
        body = {"jsonrpc": "2.0", "id": msg["id"], "result": {
            "structuredContent": {"data": {"tool_slugs": ["X"]}, "successful": True},
            "content": [{"type": "text", "text": "a summary"}]}}
        return httpx.Response(200, text=": ping\n\nevent: message\ndata: " + json.dumps(body) + "\n\n",
                              headers={"content-type": "text/event-stream"})

    fake.hook = stream
    assert bridge.execute("COMPOSIO_GET_TOOL_SCHEMAS", {"tool_slugs": ["X"]}) == {
        "ok": True, "data": {"tool_slugs": ["X"]}, "successful": True}


@pytest.mark.parametrize(("result", "reason"), [
    (text_result({"data": {}, "error": "1 out of 1 searches failed", "successful": False}),
     "1 out of 1 searches failed"),
    (text_result({"data": {}, "error": {"code": 7}, "successful": True}), '{"code": 7}'),
    (text_result({"data": {}, "error": None, "successful": False}), "Composio did not finish the call"),
    ({"isError": True, "content": [{"type": "text", "text": json.dumps({"data": {}})}]},
     "Composio did not finish the call"),
    ({"isError": True, "content": [{"type": "text", "text": "Tool not found"}]}, "Tool not found"),
])
def test_a_call_composio_did_not_finish_is_not_ok_with_its_reason(tmp_path: Path, fake: Composio,
                                                                  bridges: list[ConnectBridge],
                                                                  result: dict[str, Any], reason: str) -> None:
    bridge = connect(tmp_path, fake, bridges)
    fake.answers[SEARCH_TOOLS] = lambda args: result
    out = bridge.execute(SEARCH_TOOLS, {"queries": [{"use_case": "x"}]})
    assert out["ok"] is False and out["reason"] == reason


def test_a_401_refreshes_the_sign_in_once_and_tries_once_more(tmp_path: Path, fake: Composio,
                                                              bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)
    fake.live.discard(A1)                                 # Composio no longer takes A1 (revoked, or lapsed early)
    out = bridge.execute(SEARCH_TOOLS, {"queries": [{"use_case": "read mail"}]})
    assert out["ok"] is True and out["data"] == {"echo": {"queries": [{"use_case": "read mail"}]}}
    assert fake.bearers("tools/call") == [A1, A2] and len(fake.token_forms()) == 1
    saved = json.loads((tmp_path / "composio-connect.json").read_text())
    assert (saved["access_token"], saved["refresh_token"]) == (A2, R2)
    assert bridge.execute(SEARCH_TOOLS, {})["ok"] is True                 # A2 from now on, with no refresh
    assert fake.bearers("tools/call")[-1] == A2 and len(fake.token_forms()) == 1


def test_a_401_while_starting_refreshes_too(tmp_path: Path, fake: Composio, bridges: list[ConnectBridge]) -> None:
    sign_in(tmp_path / "composio-connect.json", access=A3)                # a token Composio does not take
    bridge = connect(tmp_path, fake, bridges, start=False)
    assert bridge.try_start()
    assert fake.bearers("initialize") == [A3, A2] and len(fake.token_forms()) == 1


def test_a_second_401_holds_the_sign_in_then_tries_it_again(tmp_path: Path, fake: Composio,
                                                          bridges: list[ConnectBridge]) -> None:
    """Composio refuses every token for a while (its 401 also covers not finding the account). That may pass: the
    model is told to try again later, Settings does not ask for a new sign-in, and once the hold is over the same
    sign-in works again."""
    now = [1_000_000.0]
    bridge = connect(tmp_path, fake, bridges, now=now)
    fake.live.discard(A1)
    fake.take_new = False                                 # Composio refuses even a fresh token
    fake.grants[R2] = (A3, R3)                            # a further refresh would mint a pair: not during the hold
    held = {"ok": False, "reason": composio_connect.NOT_RENEWED}
    assert bridge.execute(SEARCH_TOOLS, {}) == held
    assert fake.bearers("tools/call") == [A1, A2] and len(fake.token_forms()) == 1
    assert not bridge.needs_sign_in                       # nothing says the sign-in itself is gone
    sent = len(fake.requests)
    now[0] += composio_connect.REFUSED_RETRY_SECS - 1
    assert bridge.execute(SEARCH_TOOLS, {}) == held
    assert bridge.connect_link("gmail") is None and bridge.connections(max_age=0)["gmail"] is False
    assert len(fake.requests) == sent and not bridge.needs_sign_in       # held: nothing sent, no pair minted
    now[0] += 1
    fake.live.add(A2)                                     # Composio takes its tokens again
    assert bridge.execute(SEARCH_TOOLS, {})["ok"] is True
    assert fake.bearers("tools/call")[-1] == A2 and len(fake.token_forms()) == 1   # the held token, no new pair


def test_a_refusal_that_lasts_costs_one_refresh_per_hold_until_the_token_endpoint_says_no(
        tmp_path: Path, fake: Composio, bridges: list[ConnectBridge]) -> None:
    now = [1_000_000.0]
    bridge = connect(tmp_path, fake, bridges, now=now)
    fake.live.clear()
    fake.take_new = False
    fake.grants[R2] = (A3, R3)
    held = {"ok": False, "reason": composio_connect.NOT_RENEWED}
    assert bridge.execute(SEARCH_TOOLS, {}) == held
    now[0] += composio_connect.REFUSED_RETRY_SECS
    assert bridge.execute(SEARCH_TOOLS, {}) == held and not bridge.needs_sign_in
    assert fake.bearers("tools/call") == [A1, A2, A2, A3] and len(fake.token_forms()) == 2   # one refresh a round
    now[0] += composio_connect.REFUSED_RETRY_SECS
    signed_out = {"ok": False, "reason": composio_connect.SIGNED_OUT}
    assert bridge.execute(SEARCH_TOOLS, {}) == signed_out                # R3 has no grant: invalid_grant
    assert bridge.needs_sign_in and len(fake.token_forms()) == 3
    sent = len(fake.requests)
    now[0] += 86_400.0
    assert bridge.execute(SEARCH_TOOLS, {}) == signed_out and len(fake.requests) == sent   # for good
    sign_in(tmp_path / "composio-connect.json", access=A4, refresh=R4, now=now[0])   # the owner signs in again
    fake.live.add(A4)
    assert not bridge.needs_sign_in
    assert bridge.execute(SEARCH_TOOLS, {})["ok"] is True and fake.bearers("tools/call")[-1] == A4


def test_a_start_attempt_sends_nothing_while_the_sign_in_is_held(tmp_path: Path, fake: Composio,
                                                                 bridges: list[ConnectBridge]) -> None:
    now = [1_000_000.0]
    fake.live.clear()
    fake.take_new = False
    fake.grants[R2] = (A3, R3)
    bridge = connect(tmp_path, fake, bridges, now=now, start=False)
    assert not bridge.try_start() and fake.bearers("initialize") == [A1, A2] and not bridge.needs_sign_in
    sent = len(fake.requests)
    now[0] += composio_connect.REFUSED_RETRY_SECS - 1
    assert not bridge.try_start() and len(fake.requests) == sent
    now[0] += 1
    fake.live.add(A2)
    assert bridge.try_start() and fake.bearers("initialize")[-1] == A2 and len(fake.token_forms()) == 1


def test_a_refused_token_with_no_refresh_token_behind_it_needs_a_new_sign_in(tmp_path: Path, fake: Composio,
                                                                            bridges: list[ConnectBridge]) -> None:
    """Composio gave no refresh token (onboarding saves such a sign-in, which lapses with its access token). Once
    that token is refused there is nothing to renew: a new sign-in is the one way back."""
    sign_in(tmp_path / "composio-connect.json", refresh="")
    bridge = connect(tmp_path, fake, bridges)
    fake.live.discard(A1)
    signed_out = {"ok": False, "reason": composio_connect.SIGNED_OUT}
    assert bridge.execute(SEARCH_TOOLS, {}) == signed_out
    assert bridge.needs_sign_in and fake.token_forms() == []
    sent = len(fake.requests)
    assert bridge.execute(SEARCH_TOOLS, {}) == signed_out and len(fake.requests) == sent   # it is not sent again


def test_a_new_sign_in_written_while_the_old_one_is_refused_gets_its_own_try(tmp_path: Path, fake: Composio,
                                                                             bridges: list[ConnectBridge]) -> None:
    path = sign_in(tmp_path / "composio-connect.json")

    def meanwhile(request: httpx.Request) -> httpx.Response:
        if request.headers.get("authorization") == f"Bearer {A2}":
            sign_in(path, access=A3, refresh=R3)                          # the setup page signs in again, just then
            fake.live.add(A3)
        return fake.handle(request)

    bridge = ConnectBridge(ConnectTokenSource(path, transport=fake.transport),
                           transport=httpx.MockTransport(meanwhile))
    bridges.append(bridge)
    assert bridge.try_start()
    fake.live.discard(A1)
    fake.take_new = False
    # A2, fresh from the refresh, is refused too, but the file holds another sign-in by then: that one is not set aside
    assert bridge.execute(SEARCH_TOOLS, {}) == {"ok": False, "reason": composio_connect.NOT_RENEWED}
    assert not bridge.needs_sign_in
    assert bridge.execute(SEARCH_TOOLS, {})["ok"] is True
    assert fake.bearers("tools/call") == [A1, A2, A3] and len(fake.token_forms()) == 1


def test_a_refused_refresh_asks_for_a_new_sign_in_and_stops_calling(tmp_path: Path, fake: Composio,
                                                                    bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)
    fake.live.discard(A1)
    fake.grants.clear()                                   # the refresh token is gone: invalid_grant
    assert bridge.execute(SEARCH_TOOLS, {}) == {"ok": False, "reason": composio_connect.SIGNED_OUT}
    assert bridge.needs_sign_in
    sent = len(fake.requests)
    assert bridge.execute(SEARCH_TOOLS, {}) == {"ok": False, "reason": composio_connect.SIGNED_OUT}
    assert len(fake.requests) == sent                     # neither the refused token nor the dead refresh is sent


def test_a_refresh_that_may_work_later_says_try_again(tmp_path: Path, fake: Composio,
                                                      bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)
    fake.live.discard(A1)
    fake.token_reply = httpx.Response(502, text="bad gateway")
    assert bridge.execute(SEARCH_TOOLS, {}) == {"ok": False, "reason": composio_connect.NOT_RENEWED}
    assert not bridge.needs_sign_in


def test_a_token_source_with_no_refresh_gets_one_try(fake: Composio, bridges: list[ConnectBridge]) -> None:
    bridge = ConnectBridge(lambda: A1, transport=fake.transport)            # a plain callable: no refresh to call
    bridges.append(bridge)
    assert bridge.try_start() and not bridge.needs_sign_in
    fake.live.clear()
    assert bridge.execute(SEARCH_TOOLS, {}) == {"ok": False, "reason": composio_connect.NOT_RENEWED}
    assert len(fake.calls("tools/call")) == 1 and fake.token_forms() == []


def test_a_403_is_not_a_reason_to_refresh(tmp_path: Path, fake: Composio, bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)
    fake.hook = lambda msg: httpx.Response(403, json={"error": "forbidden"}) if msg.get(
        "method") == "tools/call" else None
    out = bridge.execute(SEARCH_TOOLS, {})
    assert out["ok"] is False and "HTTP 403" in out["reason"] and fake.token_forms() == []
    assert len(fake.calls("tools/call")) == 1


def test_a_call_that_timed_out_is_not_sent_again_and_may_have_run(tmp_path: Path, fake: Composio,
                                                                  bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)

    def slow(msg: dict[str, Any]) -> Optional[httpx.Response]:
        if msg.get("method") == "tools/call":
            raise httpx.ReadTimeout("timed out")
        return None

    fake.hook = slow
    args = {"tools": [{"tool_slug": "GMAIL_SEND_EMAIL", "arguments": {"to": "sam@example.com"}}],
            "sync_response_to_workbench": False}
    out = bridge.execute("COMPOSIO_MULTI_EXECUTE_TOOL", args)
    assert out["ok"] is False and out["reason"].startswith("ReadTimeout")
    assert out["reason"].endswith(composio_connect.MAY_HAVE_RUN)
    assert len(fake.calls("tools/call")) == 1             # a send that may have gone is never sent twice by code


MALFORMED: dict[str, Callable[[dict[str, Any]], httpx.Response]] = {
    "a body that is not json": lambda m: httpx.Response(200, text="<html>", headers={"content-type": "application/json"}),
    "a json array": lambda m: httpx.Response(200, json=[1, 2, 3]),
    "no reply with this id": lambda m: rpc({"id": 999}, {"content": []}),
    "an empty body": lambda m: httpx.Response(200, content=b"", headers={"content-type": "application/json"}),
    "an event stream of junk": lambda m: httpx.Response(200, text="data: {not json\n\n",
                                                        headers={"content-type": "text/event-stream"}),
    "a json-rpc error": lambda m: httpx.Response(200, json={"jsonrpc": "2.0", "id": m["id"],
                                                            "error": {"code": -32000, "message": "boom"}}),
    "an error that is a word": lambda m: httpx.Response(200, json={"jsonrpc": "2.0", "id": m["id"], "error": "boom"}),
    "a result that is a word": lambda m: rpc(m, "nope"),
    "content that is not a list": lambda m: rpc(m, {"content": 7}),
    "content parts that are not objects": lambda m: rpc(m, {"content": [1, "x", None, {"type": "text"}]}),
    "text that is not json": lambda m: rpc(m, {"content": [{"type": "text", "text": "plain words"}]}),
    "text that is a json list": lambda m: rpc(m, {"content": [{"type": "text", "text": "[1, 2]"}]}),
    "text nested past python's limit": lambda m: rpc(m, {"content": [{"type": "text",
                                                                       "text": "[" * 5000 + "]" * 5000}]}),
    "two text parts": lambda m: rpc(m, {"content": [{"type": "text", "text": "{}"}, {"type": "text", "text": "{}"}]}),
    "structured content that is a list": lambda m: rpc(m, {"structuredContent": [1, 2]}),
    "an image": lambda m: rpc(m, {"content": [{"type": "image", "data": "AAAA", "mimeType": "image/png"}]}),
    "a body nested past python's limit": lambda m: httpx.Response(200, text="[" * 100_000 + "]" * 100_000,
                                                                  headers={"content-type": "application/json"}),
    "a server error": lambda m: httpx.Response(500, text="Internal Server Error"),
    "a dropped connection": lambda m: (_ for _ in ()).throw(httpx.RemoteProtocolError("peer closed")),
}


@pytest.mark.parametrize("answer", list(MALFORMED), ids=list(MALFORMED))
def test_a_malformed_answer_never_raises(tmp_path: Path, fake: Composio, bridges: list[ConnectBridge],
                                         answer: str) -> None:
    bridge = connect(tmp_path, fake, bridges)
    fake.hook = lambda msg: MALFORMED[answer](msg) if msg.get("method") == "tools/call" else None
    for name in (SEARCH_TOOLS, MANAGE_CONNECTIONS, "COMPOSIO_REMOTE_WORKBENCH"):
        out = bridge.execute(name, {"toolkits": ["gmail"]})
        assert isinstance(out, dict) and isinstance(out["ok"], bool)
        assert out["ok"] or isinstance(out["reason"], str) and out["reason"]
        json.dumps(out)                                   # what the brain hands back to the model
    assert bridge.connect_link("gmail") is None
    assert bridge.connections(max_age=0) == dict.fromkeys(composio_connect.APPS, False)
    assert fake.token_forms() == []


def test_a_result_with_two_text_parts_is_read_as_its_text(tmp_path: Path, fake: Composio,
                                                          bridges: list[ConnectBridge]) -> None:
    """The model reads every part: of two, neither is dropped. Settings reads the one part that is Composio's object,
    so a note after it (as Composio sends since 2026-10-14) does not make a connected app read as not connected."""
    bridge = connect(tmp_path, fake, bridges)
    first = json.dumps({"data": {"toolkit_connection_statuses": [{"toolkit": "gmail", "has_active_connection": True}]},
                        "successful": True})
    fake.answers[SEARCH_TOOLS] = lambda args: {"content": [{"type": "text", "text": first},
                                                           {"type": "text", "text": "and a note"}]}
    assert bridge.execute(SEARCH_TOOLS, {}) == {"ok": True, "result": first + "\nand a note"}
    assert bridge.connections(["gmail"]) == {"gmail": True}    # seen in the brain's search: no call of its own
    assert len(fake.calls("tools/call")) == 1
    assert bridge.connections(["gmail"], max_age=0) == {"gmail": True}  # Settings' own read, the same answer
    assert len(fake.calls("tools/call")) == 2


def test_two_json_objects_in_one_result_are_read_as_neither(tmp_path: Path, fake: Composio,
                                                            bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)
    one = json.dumps({"data": {"toolkit_connection_statuses": [{"toolkit": "gmail", "has_active_connection": True}]}})
    fake.answers[SEARCH_TOOLS] = lambda args: {"content": [{"type": "text", "text": one},
                                                           {"type": "text", "text": one}]}
    assert bridge.connections(["gmail"], max_age=0) == {"gmail": False}


def test_an_answer_that_cannot_be_read_after_the_call_ran_says_it_may_have_run(
        tmp_path: Path, fake: Composio, bridges: list[ConnectBridge], monkeypatch: pytest.MonkeyPatch) -> None:
    bridge = connect(tmp_path, fake, bridges)
    fake.answers[SEARCH_TOOLS] = lambda args: search_answer(gmail=True)

    def broken(*_: Any) -> Any:
        raise RuntimeError("a reader bug")

    # Settings' bookkeeping failing costs the model nothing: the answer is the answer
    monkeypatch.setattr(composio_connect, "search_statuses", broken)
    out = bridge.execute(SEARCH_TOOLS, {})
    assert out["ok"] is True and out["data"]["toolkit_connection_statuses"][0]["toolkit"] == "gmail"
    # the answer itself not read: never a raise, never a plain failure the model would run again
    monkeypatch.setattr(composio_connect, "result_for_model", broken)
    assert bridge.execute(SEARCH_TOOLS, {}) == {"ok": False, "reason": composio_connect.UNREADABLE}
    assert len(fake.calls("tools/call")) == 2             # each call made once


@pytest.mark.parametrize(("name", "args"), [
    (None, {}), (["COMPOSIO_SEARCH_TOOLS"], {}), ("COMPOSIO_SEARCH_WEB", {}), ("GMAIL_SEND_EMAIL", {}),
    (SEARCH_TOOLS, ["not", "an", "object"]), (SEARCH_TOOLS, "text"),
])
def test_a_call_eve_cannot_make_is_a_plain_answer_with_no_request(tmp_path: Path, fake: Composio,
                                                                 bridges: list[ConnectBridge], name: Any,
                                                                 args: Any) -> None:
    bridge = connect(tmp_path, fake, bridges)
    sent = len(fake.requests)
    out = bridge.execute(name, args)
    assert out["ok"] is False and out["reason"] and len(fake.requests) == sent


def test_no_arguments_are_an_empty_object(tmp_path: Path, fake: Composio, bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)
    assert bridge.execute(SEARCH_TOOLS, None)["ok"] is True                # type: ignore[arg-type]
    assert fake.calls("tools/call")[-1]["params"]["arguments"] == {}


def test_a_huge_answer_is_clipped_as_a_direct_apps_is(tmp_path: Path, fake: Composio,
                                                      bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)
    fake.answers[SEARCH_TOOLS] = lambda args: text_result({"data": {"blob": "x" * 200_000}, "successful": True})
    out = bridge.execute(SEARCH_TOOLS, {})
    assert out["ok"] is True and out["clipped"] is True
    assert len(out["result"]) == apps_mcp.RESULT_CLIP + 1 and out["result"].endswith("…")


def test_start_returns_at_once_and_keeps_trying_until_composio_answers(tmp_path: Path, fake: Composio,
                                                                       bridges: list[ConnectBridge]) -> None:
    failures = [2]
    gate = threading.Event()

    def flaky(msg: dict[str, Any]) -> Optional[httpx.Response]:
        if msg.get("method") == "initialize":
            gate.wait(5)
            if failures[0]:
                failures[0] -= 1
                return httpx.Response(503, text="starting")
        return None

    fake.hook = flaky
    bridge = connect(tmp_path, fake, bridges, start=False, backoff=(0.01,))
    bridge.start()
    first = bridge._thread
    bridge.start()                                        # while it runs: nothing new
    assert bridge._thread is first and not bridge.started
    gate.set()
    assert wait_until(lambda: bridge.started)
    assert len(fake.calls("initialize")) == 3 and bridge.names == frozenset(META_TOOLS)
    assert wait_until(lambda: not first.is_alive())
    bridge.start()                                        # once up: nothing new
    assert bridge._thread is first


def test_close_stops_the_tries(tmp_path: Path, fake: Composio, bridges: list[ConnectBridge]) -> None:
    fake.hook = lambda msg: httpx.Response(503) if msg.get("method") == "initialize" else None
    bridge = connect(tmp_path, fake, bridges, start=False, backoff=(0.01,))
    bridge.start()
    assert wait_until(lambda: len(fake.calls("initialize")) >= 2)
    thread = bridge._thread
    bridge.close()
    assert thread is not None and wait_until(lambda: not thread.is_alive())
    tried = len(fake.calls("initialize"))
    time.sleep(0.1)
    assert len(fake.calls("initialize")) == tried and not bridge.started
    bridge.start()
    assert bridge._thread is thread                       # closed: never starts again


# ---- Settings → Apps: Connect Links and the connected apps -----------------------------------------------------

def manage_answer(results: Any, **summary: int) -> dict[str, Any]:
    """COMPOSIO_MANAGE_CONNECTIONS's documented envelope (docs.composio.dev meta-tools page, 2026-10-13). Composio
    documents no shape for a toolkit's own part, so the parts here are assumptions the bridge reads loosely."""
    data: dict[str, Any] = {"message": "done", "results": results}
    if summary:
        data["summary"] = summary
    return text_result({"data": data, "error": None, "successful": True})


def test_connect_link_asks_for_one_toolkit_in_the_shape_that_needs_no_yes(tmp_path: Path, fake: Composio,
                                                                          bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)
    fake.answers[MANAGE_CONNECTIONS] = lambda args: manage_answer(
        {"gmail": {"toolkit": "gmail", "status": "initiated", "redirect_url": LINK}},
        total_toolkits=1, active_connections=0, initiated_connections=1, failed_connections=0)
    assert bridge.connect_link(" Gmail ") == LINK
    [call] = fake.calls("tools/call")
    assert call["params"] == {"name": MANAGE_CONNECTIONS, "arguments": {"toolkits": ["gmail"]}}
    assert composio_tools.manage_needs_yes(call["params"]["arguments"]) is False   # control: it only checks or links
    assert composio_tools.manage_needs_yes({"toolkits": ["gmail"], "reinitiate_all": True}) is True


SLACK_LINK = "https://connect.composio.dev/link/ln_slack"
NOTION_LINK = "https://connect.composio.dev/link/ln_notion"


@pytest.mark.parametrize("answer", [
    manage_answer([{"toolkit": "GMAIL", "redirectUrl": LINK}]),                     # a list of parts
    manage_answer({"gmail_v2": {"redirect_url": LINK, "status": "INITIATED"}}),    # one part, under another key
    text_result({"data": {"connection": {"auth": {"redirect_url": LINK}}}, "successful": True}),   # nested
    {"structuredContent": {"data": {"results": {"gmail": {"connect_link": LINK}}}}},
    manage_answer({"gmail": {"status": "initiated", "connection": {"redirect_url": LINK}}}),   # nested in its part
    manage_answer({"slack": {"redirect_url": SLACK_LINK}, "gmail": {"redirect_url": LINK},
                   "notion": {"redirect_url": NOTION_LINK}}),                       # its part beside others'
    manage_answer([{"toolkit": "slack", "redirect_url": SLACK_LINK}, {"toolkit": "gmail", "redirect_url": LINK}]),
    text_result({"data": {"redirect_url": LINK, "auth": {"redirect_url": LINK}}, "successful": True}),   # said twice
])
def test_connect_link_reads_the_link_from_other_answer_shapes(tmp_path: Path, fake: Composio,
                                                              bridges: list[ConnectBridge], answer: Any) -> None:
    bridge = connect(tmp_path, fake, bridges)
    fake.answers[MANAGE_CONNECTIONS] = lambda args: answer
    assert bridge.connect_link("gmail") == LINK


@pytest.mark.parametrize("link", [
    "https://connect.composio.dev.example.com/link/ln_1", "http://connect.composio.dev/link/ln_1",
    "https://example.com\\@connect.composio.dev/link", "javascript:alert(1)",
    "https://[evil/x", "https://connect.composio.dev]/x",   # Python cannot split these: None, not a raise
])
def test_connect_link_refuses_a_link_that_is_not_composios(tmp_path: Path, fake: Composio,
                                                           bridges: list[ConnectBridge], link: str) -> None:
    bridge = connect(tmp_path, fake, bridges)
    fake.answers[MANAGE_CONNECTIONS] = lambda args: manage_answer({"gmail": {"redirect_url": link}})
    assert bridge.connect_link("gmail") is None
    assert len(fake.calls("tools/call")) == 1             # it was asked: the link it sent is what is refused


@pytest.mark.parametrize("answer", [
    manage_answer({"gmail": {"toolkit": "gmail", "status": "ACTIVE", "connected_account_id": "ca_1"}}),
    manage_answer({"gmail": {"has_active_connection": True}}),
    manage_answer({"gmail_v2": {"status": "ACTIVE"}}),    # one toolkit was asked about: its one part, whatever its key
    manage_answer({"gmail": {"connected_account_id": "ca_1"}}, total_toolkits=1, active_connections=1,
                  initiated_connections=0),               # says nothing itself: the summary says it
])
def test_connect_link_for_an_app_already_connected_is_none_and_settings_shows_it(
        tmp_path: Path, fake: Composio, bridges: list[ConnectBridge], answer: dict[str, Any]) -> None:
    bridge = connect(tmp_path, fake, bridges)
    fake.answers[MANAGE_CONNECTIONS] = lambda args: answer
    assert bridge.connect_link("gmail") is None
    assert bridge.connections(["gmail"]) == {"gmail": True}
    assert len(fake.calls("tools/call")) == 1             # known from the check: no search


@pytest.mark.parametrize("answer", [
    manage_answer({"slack": {"status": "ACTIVE"}, "notion": {"status": "ACTIVE"}}),
    manage_answer({"slack": {"status": "initiated", "redirect_url": SLACK_LINK},   # other apps' links: opening one
                   "notion": {"status": "initiated", "redirect_url": NOTION_LINK}}),   # would connect that app
    manage_answer([{"toolkit": "slack", "redirect_url": SLACK_LINK},
                   {"toolkit": "notion", "redirect_url": NOTION_LINK}]),               # a list naming other apps
    text_result({"data": {"results": {"slack": {"status": "ACTIVE"}, "notion": {"status": "ACTIVE"}},
                          "redirect_url": SLACK_LINK}, "successful": True}),         # a link outside every part
    manage_answer({"slack": {"connected_account_id": "ca_1"}, "notion": {}},
                  total_toolkits=1, active_connections=1),                           # a summary of other apps
    text_result({"data": {"results": [{"redirect_url": SLACK_LINK}, {"auth": {"redirect_url": NOTION_LINK}}]},
                 "successful": True}),                                               # links that name no app
])
def test_connect_link_reads_nothing_into_an_answer_about_other_apps(tmp_path: Path, fake: Composio,
                                                                    bridges: list[ConnectBridge], answer: Any) -> None:
    bridge = connect(tmp_path, fake, bridges)
    fake.answers[MANAGE_CONNECTIONS] = lambda args: answer
    fake.answers[SEARCH_TOOLS] = lambda args: search_answer(gmail=False)
    assert bridge.connect_link("gmail") is None
    assert bridge.connections(["gmail"]) == {"gmail": False}
    assert len(fake.calls("tools/call")) == 2             # nothing learned of gmail from that check: the search reads it


def test_after_a_connect_link_settings_reads_that_app_again(tmp_path: Path, fake: Composio,
                                                            bridges: list[ConnectBridge]) -> None:
    now = [100.0]
    bridge = connect(tmp_path, fake, bridges, clock=lambda: now[0])
    fake.answers[SEARCH_TOOLS] = lambda args: search_answer(gmail=False)
    assert bridge.connections(["gmail"]) == {"gmail": False}
    fake.answers[MANAGE_CONNECTIONS] = lambda args: manage_answer(
        {"gmail": {"status": "initiated", "redirect_url": LINK}})
    assert bridge.connect_link("gmail") == LINK
    fake.answers[SEARCH_TOOLS] = lambda args: search_answer(gmail=True)  # the owner follows the link
    assert bridge.connections(["gmail"]) == {"gmail": True}              # read again now, not trusted for a minute
    assert [c["params"]["name"] for c in fake.calls("tools/call")] == [SEARCH_TOOLS, MANAGE_CONNECTIONS,
                                                                        SEARCH_TOOLS]


def test_a_connect_link_never_waits_behind_a_slow_search_or_a_slow_brain_call(tmp_path: Path, fake: Composio,
                                                                            bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)
    release, inside = threading.Event(), {SEARCH_TOOLS: threading.Event(), "COMPOSIO_REMOTE_WORKBENCH": threading.Event()}

    def slow(name: str) -> Callable[[dict[str, Any]], Any]:
        def answer(args: dict[str, Any]) -> Any:
            inside[name].set()
            release.wait(5)
            return search_answer(gmail=True)
        return answer

    fake.answers[SEARCH_TOOLS] = slow(SEARCH_TOOLS)
    fake.answers["COMPOSIO_REMOTE_WORKBENCH"] = slow("COMPOSIO_REMOTE_WORKBENCH")
    fake.answers[MANAGE_CONNECTIONS] = lambda args: manage_answer({"gmail": {"redirect_url": LINK}})
    busy = [threading.Thread(target=bridge.connections),
            threading.Thread(target=bridge.execute, args=("COMPOSIO_REMOTE_WORKBENCH", {"code_to_execute": "1"}))]
    for thread in busy:
        thread.start()
    try:
        assert all(event.wait(5) for event in inside.values())   # Settings' read and the brain's cell both running
        began = time.monotonic()
        assert bridge.connect_link("gmail") == LINK
        assert time.monotonic() - began < 2
    finally:
        release.set()
        for thread in busy:
            thread.join(5)


@pytest.mark.parametrize("toolkit", ["../gmail", "gmail;rm", "", None, "exa", "composio_search", "browser_tool"])
def test_connect_link_makes_no_request_for_a_bad_slug_or_a_web_toolkit(tmp_path: Path, fake: Composio,
                                                                       bridges: list[ConnectBridge],
                                                                       toolkit: Any) -> None:
    bridge = connect(tmp_path, fake, bridges)
    sent = len(fake.requests)
    assert bridge.connect_link(toolkit) is None and len(fake.requests) == sent


def search_answer(**connected: bool) -> dict[str, Any]:
    """COMPOSIO_SEARCH_TOOLS's documented answer (docs.composio.dev meta-tools page, 2026-10-13), trimmed."""
    return text_result({"data": {
        "results": [], "tool_schemas": {}, "time_info": {}, "session": {"id": "s", "generate_id": True},
        "next_steps_guidance": [],
        "toolkit_connection_statuses": [
            {"toolkit": kit, "description": "", "has_active_connection": on, "connection_details": {},
             "status_message": "ok"} for kit, on in connected.items()]},
        "error": None, "successful": True})


def test_connections_reads_the_search_and_trusts_it_for_a_minute(tmp_path: Path, fake: Composio,
                                                                 bridges: list[ConnectBridge]) -> None:
    now = [100.0]
    bridge = connect(tmp_path, fake, bridges, clock=lambda: now[0])
    fake.answers[SEARCH_TOOLS] = lambda args: search_answer(gmail=True, slack=False, outlook=True)
    assert bridge.connections() == {**dict.fromkeys(composio_connect.APPS, False), "gmail": True}
    [call] = fake.calls("tools/call")
    args = call["params"]["arguments"]
    assert call["params"]["name"] == SEARCH_TOOLS and args["session"] == {"generate_id": True}
    assert len(args["queries"]) == len(composio_connect.APPS)
    for query, label in zip(args["queries"], composio_connect.APPS.values(), strict=True):
        assert label in query["use_case"]                 # each app named, so the answer reports it
    assert list(bridge.connections()) == list(composio_connect.APPS)
    assert len(fake.calls("tools/call")) == 1             # trusted for CONNECTIONS_TTL_SECS
    now[0] += composio_connect.CONNECTIONS_TTL_SECS
    fake.answers[SEARCH_TOOLS] = lambda args: search_answer(gmail=False, slack=True)
    assert bridge.connections(["slack", "gmail", "slack"]) == {"slack": True, "gmail": False}
    assert bridge.connections(["notion"], max_age=0) == {"notion": False}
    assert len(fake.calls("tools/call")) == 3
    assert fake.calls("tools/call")[-1]["params"]["arguments"]["queries"] == [
        {"use_case": "search my pages in Notion"}]


def test_connections_reads_many_apps_a_few_at_a_time(tmp_path: Path, fake: Composio,
                                                    bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)
    kits = [f"kit{n}" for n in range(composio_connect.LOOK_MAX * 2 + 3)]
    fake.answers[SEARCH_TOOLS] = lambda args: search_answer(kit0=True, kit22=True)
    assert bridge.connections(kits) == {kit: kit in ("kit0", "kit22") for kit in kits}
    sizes = [len(c["params"]["arguments"]["queries"]) for c in fake.calls("tools/call")]
    assert sizes == [composio_connect.LOOK_MAX, composio_connect.LOOK_MAX, 3]
    assert fake.calls("tools/call")[0]["params"]["arguments"]["queries"][0] == {"use_case": "list my recent items in kit0"}


def test_connections_keeps_what_was_known_while_composio_fails(tmp_path: Path, fake: Composio,
                                                               bridges: list[ConnectBridge]) -> None:
    now = [100.0]
    bridge = connect(tmp_path, fake, bridges, clock=lambda: now[0])
    fake.answers[SEARCH_TOOLS] = lambda args: search_answer(gmail=True)
    assert bridge.connections(["gmail"]) == {"gmail": True}
    now[0] += composio_connect.CONNECTIONS_TTL_SECS
    fake.answers[SEARCH_TOOLS] = lambda args: httpx.Response(500, text="down")
    assert bridge.connections(["gmail", "slack"]) == {"gmail": True, "slack": False}
    assert bridge.connections(["gmail", "slack"]) == {"gmail": True, "slack": False}
    assert len(fake.calls("tools/call")) == 2             # no new read for LOOK_RETRY_SECS after a failed one
    assert bridge.connections(None) == {} and bridge.connections(7) == {}   # type: ignore[arg-type]


def test_what_the_brain_runs_updates_what_settings_shows(tmp_path: Path, fake: Composio,
                                                         bridges: list[ConnectBridge]) -> None:
    now = [100.0]
    bridge = connect(tmp_path, fake, bridges, clock=lambda: now[0])
    fake.answers[SEARCH_TOOLS] = lambda args: search_answer(gmail=True, notion=False)
    assert bridge.execute(SEARCH_TOOLS, {"queries": [{"use_case": "read my mail"}]})["ok"] is True
    fake.answers[MANAGE_CONNECTIONS] = lambda args: manage_answer(
        {"slack": {"status": "initiated", "redirect_url": LINK}, "linear": {"status": "ACTIVE"}})
    assert bridge.execute(MANAGE_CONNECTIONS, {"toolkits": ["slack", "linear"]})["ok"] is True
    calls = len(fake.calls("tools/call"))
    assert bridge.connections(["gmail", "notion", "linear"]) == {"gmail": True, "notion": False, "linear": True}
    assert len(fake.calls("tools/call")) == calls         # all three known from the brain's own calls
    fake.answers[SEARCH_TOOLS] = lambda args: search_answer(slack=True)
    assert bridge.connections(["slack"]) == {"slack": True}   # a link was just sent for it: read again
    assert len(fake.calls("tools/call")) == calls + 1


# ---- secrets and the brain's duck type -------------------------------------------------------------------------

def test_no_token_reaches_a_log_a_result_or_a_repr(tmp_path: Path, fake: Composio, bridges: list[ConnectBridge],
                                                   caplog: Any) -> None:
    caplog.set_level(logging.DEBUG)
    bridge = connect(tmp_path, fake, bridges, start=False)
    seen: list[Any] = [bridge.try_start()]
    fake.live.discard(A1)                                 # a 401, a refresh, a retry
    seen.append(bridge.execute(SEARCH_TOOLS, {"queries": [{"use_case": "read mail"}]}))
    fake.answers[MANAGE_CONNECTIONS] = lambda args: manage_answer({"gmail": {"redirect_url": LINK}})
    seen.append(bridge.connect_link("gmail"))
    fake.live.clear()                                     # then every token refused, and no refresh left
    seen += [bridge.execute(SEARCH_TOOLS, {}), bridge.connect_link("slack"), bridge.connections(max_age=0)]
    sign_in(tmp_path / "composio-connect.json", access=A3, refresh=R3)   # a new sign-in, whose fresh token is
    fake.grants[R3] = (A4, R4)                                           # refused too: it is held (hold)
    fake.take_new = False
    seen += [bridge.execute(SEARCH_TOOLS, {}), bridge.needs_sign_in, bridge.connect_link("slack")]
    assert seen[-3]["reason"] == composio_connect.NOT_RENEWED and seen[-2] is False
    seen += [repr(bridge), repr(bridge._tokens), bridge.tools()]
    assert CANARY not in json.dumps(seen, default=str) and CANARY not in caplog.text
    # controls: the tokens were really there to leak, and this log capture sees this module's lines
    assert any(CANARY in r.headers.get("authorization", "") for r in fake.requests)
    assert CANARY in (tmp_path / "composio-connect.json").read_text()
    logging.getLogger("eve_bridge.composio_connect").debug("control %s", A2)
    assert CANARY in caplog.text


def test_the_bridge_has_the_duck_type_the_brain_reads(tmp_path: Path, fake: Composio,
                                                      bridges: list[ConnectBridge]) -> None:
    bridge = connect(tmp_path, fake, bridges)
    # what telegram.TelegramInlet reads of its ``apps`` (composio_tools.ComposioBridge's shape)
    assert isinstance(bridge.started, bool) and isinstance(bridge.names, frozenset)
    assert {t["name"] for t in bridge.tools()} == bridge.names
    assert all(t["type"] == "function" and isinstance(t["parameters"], dict) for t in bridge.tools())
    assert profile.filter_tools(bridge.tools()) == bridge.tools()          # eve lite offers all of them
    assert isinstance(bridge.execute(SEARCH_TOOLS, {}), dict) and bridge.accounts_note() == ""
