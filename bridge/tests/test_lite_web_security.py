"""lite_web.py's checks (docs/plans/eve-lite.md, M4): the Host allowlist, Origin on anything but a GET, the content
type, the device cookie and its exact flags, the OAuth callbacks' single-use state, that no JSON answer can carry a
key, the security headers, what reaches LinkChat, and requests that are not plain HTTP/1.1.

Every refusal here has its positive control beside it: the same request with the one thing fixed goes through."""
from __future__ import annotations

import asyncio
import json
import re
import stat
from pathlib import Path
from typing import Any, Optional
from urllib.parse import parse_qs, urlsplit

import pytest
from test_link_chat import FakeInlet, events, store_at, two_lines
from test_lite_web_local import (
    CANARY_KEY,
    CANARY_TOKEN,
    FakeBridge,
    Rig,
    cookie_of,
    link_door,
    run,
    send_raw,
    until,
)

from eve_bridge import envfile, lite_web, onboarding, telegram, web_auth

# What a key, a token or a code looks like in a JSON answer: the canaries, OpenRouter's and OpenAI's key shapes, a
# JWT, and any long opaque run.
KEY_SHAPED = re.compile(r"CANARY-NOT-A-KEY|sk-or-v1-|sk-[A-Za-z0-9]{16,}|eyJ[A-Za-z0-9_-]{10,}|[A-Za-z0-9_+/=-]{32,}")


def key_shaped(text: str) -> list[str]:
    return KEY_SHAPED.findall(text)


@pytest.fixture(autouse=True)
def _plain_logging():
    """The sign-ins hand their key or token to telegram.hide_token, which redacts it in every log record for the
    rest of the process: undone after each test, as test_onboarding does."""
    yield
    telegram.unhide_tokens()


# ---- Host --------------------------------------------------------------------------------------------------------

def test_a_foreign_host_is_421_on_both_doors(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            mac = f"eve_mac={await r.login()}"
            p = r.port
            for host in ("evil.example", f"evil.example:{p}", f"localhost:{p + 1}", f"localhost.:{p}",
                         f"127.0.0.2:{p}", f"[::2]:{p}", f"localhost:{p}@evil.example", "localhost", ""):
                for path in ("/", lite_web.STATUS_PATH):                    # the page and a cookie route alike
                    reply = await r.mac("GET", path, host=host, cookie=mac)
                    assert reply.status == 421, (host, path, reply.status)
                    assert reply.json() == {"ok": False, "error": lite_web.WRONG_HOST} and reply.cookies == []
            # a DNS-rebinding POST: the right Origin for its own name, the wrong Host for eve
            rebound = await r.mac("POST", lite_web.PAIR_OPEN_PATH, {}, host=f"evil.example:{p}", cookie=mac)
            assert rebound.status == 421 and not r.ctx.pairing.is_open
            for host in (f"localhost:{p}", f"127.0.0.1:{p}", f"[::1]:{p}", f"LOCALHOST:{p}"):   # positive control
                assert (await r.mac("GET", lite_web.STATUS_PATH, host=host, cookie=mac)).status == 200, host
            await r.open_pairing(mac.split("=", 1)[1])
            lan = r.ctx.shared.lan.port
            assert (await r.lan("GET", "/", host=f"evil.example:{lan}")).status == 421
            assert (await r.lan("GET", "/", host=f"localhost:{p}")).status == 421     # the Mac page's port
            assert (await r.lan("GET", "/", host=f"localhost:{lan}")).status == 200

    run(go())


# ---- Origin, the body's type ---------------------------------------------------------------------------------------

def post_routes(r: Rig) -> list[tuple[str, Any, Optional[str]]]:
    """Every POST the Mac's page has: (path, body, content type)."""
    return [(lite_web.MAC_LOGIN_PATH, {"code": r.ctx.mac_login.issue()}, None),
            (lite_web.COMPOSIO_SKIP_PATH, {}, None), (lite_web.PAIR_OPEN_PATH, {}, None),
            (lite_web.REVOKE_PATH, {"id": "all"}, None), (lite_web.LAN_PATH, {"on": True}, None),
            (lite_web.RTC_PATH, {"sdp": "v=0\r\n"}, None), (lite_web.LOGOUT_PATH, {}, None),
            ("/api/web/apps/gmail/connect", {}, None),
            ("/api/web/link/turn", {"text": "hi", "client_id": "csrf-1"}, None),
            ("/api/web/link/upload", b"notes\n", "application/octet-stream")]


def test_a_post_needs_the_pages_own_origin_and_a_get_does_not(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), two_lines)
        async with Rig(tmp_path / "rig", link=link_door(tmp_path, inlet), apps=FakeBridge()) as r:
            token = await r.login()
            mac = f"eve_mac={token}"
            p = r.port
            for path, body, ctype in post_routes(r):
                for origin in (None, "http://evil.example", "null", f"http://localhost:{p + 1}",
                               f"https://localhost:{p}", f"http://localhost:{p}.evil.example", ""):
                    reply = await r.mac("POST", path, body, origin=origin, ctype=ctype, cookie=mac)
                    assert reply.status == 403, (path, origin, reply.status)
                    assert reply.json() == {"ok": False, "error": lite_web.WRONG_ORIGIN}
            # nothing happened: no window, no device revoked, the door still off, nothing heard, still signed in
            assert not r.ctx.pairing.is_open and len(r.ctx.devices.list()) == 1
            assert not r.local.lan_on() and inlet.heard == [] and r.rtc.offers == []
            assert not r.ctx.shared.saved.composio_skipped
            # positive controls: a GET needs no Origin; the same POST from the page's own origin goes through
            assert (await r.mac("GET", lite_web.STATUS_PATH, cookie=mac, origin=None)).status == 200
            assert (await r.mac("GET", lite_web.STATUS_PATH, cookie=mac, origin="http://evil.example")).status == 200
            assert (await r.mac("POST", lite_web.COMPOSIO_SKIP_PATH, {}, cookie=mac)).status == 200
            assert (await r.mac("POST", lite_web.RTC_PATH, {"sdp": "v=0\r\n"}, cookie=mac,
                                origin=f"http://LOCALHOST:{p}")).status == 200
            # the LAN door's own origin is https
            phone = await r.pair(token)
            lan = r.ctx.shared.lan.port
            for origin in (None, f"http://localhost:{lan}", f"https://localhost:{p}", "https://evil.example"):
                reply = await r.lan("POST", lite_web.RTC_PATH, {"sdp": "v=0\r\n"}, cookie=f"eve_dev={phone}",
                                    origin=origin)
                assert reply.status == 403, origin
            assert (await r.lan("POST", lite_web.RTC_PATH, {"sdp": "v=0\r\n"}, cookie=f"eve_dev={phone}")).status == 200

    run(go())


def test_a_post_body_must_be_json_or_an_upload_of_bytes(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), two_lines)
        async with Rig(tmp_path / "rig", link=link_door(tmp_path, inlet)) as r:
            mac = f"eve_mac={await r.login()}"
            for ctype in ("text/plain", "application/x-www-form-urlencoded", "multipart/form-data; boundary=x",
                          "", "application/jsonx", "application/octet-stream"):
                reply = await r.mac("POST", lite_web.COMPOSIO_SKIP_PATH, b"{}", ctype=ctype, cookie=mac)
                assert reply.status == 415, (ctype, reply.status)
            for ctype in ("application/json", "text/plain", ""):
                reply = await r.mac("POST", "/api/web/link/upload", b"notes\n", ctype=ctype, cookie=mac,
                                    headers={"X-Filename": "notes.txt"})
                assert reply.status == 415, (ctype, reply.status)
            assert not r.ctx.shared.saved.composio_skipped and inlet.heard == []
            chunked = await r.mac("POST", lite_web.COMPOSIO_SKIP_PATH, b"2\r\n{}\r\n0\r\n\r\n", cookie=mac,
                                  headers={"Transfer-Encoding": "chunked"})
            assert chunked.status in (400, 411)
            for raw, status in ((b"[1, 2]", 400), (b"{not json", 400), (b"x" * (lite_web.MAX_JSON_BYTES + 1), 413)):
                reply = await r.mac("POST", lite_web.COMPOSIO_SKIP_PATH, raw, cookie=mac)
                assert reply.status == status, (raw[:10], reply.status)
            assert not r.ctx.shared.saved.composio_skipped
            # positive controls
            assert (await r.mac("POST", lite_web.COMPOSIO_SKIP_PATH, b"{}", ctype="application/json; charset=utf-8",
                                cookie=mac)).status == 200
            up = await r.mac("POST", "/api/web/link/upload", b"notes\n", ctype="application/octet-stream",
                             cookie=mac, headers={"X-Filename": "notes.txt"})
            assert up.status == 200 and up.json()["ok"] is True

    run(go())


# ---- the cookie --------------------------------------------------------------------------------------------------

COOKIE_GETS = (lite_web.STATUS_PATH, lite_web.APPS_PATH, "/api/web/link/messages", "/api/web/link/file/" + "a" * 32)
MAC_COOKIE_GETS = (lite_web.ONBOARD_STATE_PATH, lite_web.DEVICES_PATH, lite_web.OPENROUTER_START_PATH,
                   lite_web.COMPOSIO_START_PATH)
MAC_COOKIE_POSTS = (lite_web.COMPOSIO_SKIP_PATH, lite_web.PAIR_OPEN_PATH, lite_web.REVOKE_PATH, lite_web.LAN_PATH)
COOKIE_POSTS = (lite_web.RTC_PATH, lite_web.LOGOUT_PATH, "/api/web/apps/gmail/connect", "/api/web/link/turn")


def test_a_missing_or_wrong_cookie_is_401_on_every_cookie_route(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), two_lines)
        async with Rig(tmp_path / "rig", link=link_door(tmp_path, inlet), apps=FakeBridge()) as r:
            mac_token = await r.login()
            phone = await r.pair(mac_token)
            bodies = {lite_web.REVOKE_PATH: {"id": "all"}, lite_web.LAN_PATH: {"on": False},
                      lite_web.RTC_PATH: {"sdp": "v=0\r\n"}, "/api/web/link/turn": {"text": "hi", "client_id": "c-401"}}
            for cookie in (None, f"eve_dev={phone}", f"eve_mac={'x' * 43}", f"eve_mac={mac_token[:-1]}",
                           f"other={mac_token}"):
                for path in COOKIE_GETS + MAC_COOKIE_GETS:
                    reply = await r.mac("GET", path, cookie=cookie)
                    assert reply.status == 401, (cookie, path, reply.status)
                    assert reply.json() == {"ok": False, "error": lite_web.SIGNED_OUT_MAC}
                for path in COOKIE_POSTS + MAC_COOKIE_POSTS:
                    reply = await r.mac("POST", path, bodies.get(path, {}), cookie=cookie)
                    assert reply.status == 401, (cookie, path, reply.status)
            for cookie in (None, f"eve_mac={mac_token}", f"eve_dev={phone[:-1]}"):    # the LAN door's own name only
                for path in COOKIE_GETS:
                    reply = await r.lan("GET", path, cookie=cookie)
                    assert reply.status == 401 and reply.json()["error"] == lite_web.SIGNED_OUT_DEV, (cookie, path)
                for path in COOKIE_POSTS:
                    assert (await r.lan("POST", path, bodies.get(path, {}), cookie=cookie)).status == 401
            # nothing reached: no turn heard, no peer, no window, devices as they were
            assert inlet.heard == [] and r.rtc.offers == [] and len(r.ctx.devices.list()) == 2
            assert r.local.lan_on()
            # positive controls: each door's own cookie
            assert (await r.mac("GET", lite_web.STATUS_PATH, cookie=f"eve_mac={mac_token}")).status == 200
            assert (await r.lan("GET", lite_web.STATUS_PATH, cookie=f"eve_dev={phone}")).status == 200
            assert (await r.mac("GET", lite_web.STATUS_PATH,
                                cookie=f"theme=dark; eve_mac={mac_token}; x=1")).status == 200
            # a revoked device's cookie is refused and cleared
            r.ctx.devices.revoke("all")
            gone = await r.lan("GET", lite_web.STATUS_PATH, cookie=f"eve_dev={phone}")
            assert gone.status == 401
            assert gone.cookies == ["eve_dev=; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=0"]

    run(go())


def test_a_phone_token_as_eve_mac_reaches_no_mac_only_route(tmp_path: Path) -> None:
    """A device paired on the LAN door gets an eve_dev token. Sent back as the Mac page's eve_mac on the loopback
    door (open to every account on the Mac), it is refused on every Mac-only route: the device is bound to the door
    it paired through (web_auth.Devices). The attack the spec's M4 forbids: a second account's paired browser
    redoing the OpenRouter or Composio sign-ins, or revoking the owner's devices."""
    async def go() -> None:
        async with Rig(tmp_path, apps=FakeBridge()) as r:
            mac_token = await r.login()
            phone = await r.pair(mac_token)                       # a phone's eve_dev token
            as_mac = f"eve_mac={phone}"
            bodies = {lite_web.REVOKE_PATH: {"id": "all"}, lite_web.LAN_PATH: {"on": False},
                      lite_web.PAIR_OPEN_PATH: {}, lite_web.COMPOSIO_SKIP_PATH: {}}
            for path in MAC_COOKIE_GETS:
                reply = await r.mac("GET", path, cookie=as_mac)
                assert reply.status == 401, (path, reply.status)
                assert reply.json() == {"ok": False, "error": lite_web.SIGNED_OUT_MAC}
            for path in MAC_COOKIE_POSTS:
                assert (await r.mac("POST", path, bodies.get(path, {}), cookie=as_mac)).status == 401, path
            # the sign-in redirects, which carry the owner's accounts to the provider, never start with this token
            for path in (lite_web.OPENROUTER_START_PATH, lite_web.COMPOSIO_START_PATH):
                assert (await r.mac("GET", path, cookie=as_mac)).status == 401, path
            # nothing moved: the owner's devices are intact, the LAN door still on, no new window
            assert {d["id"] for d in r.ctx.devices.list()}  # still two
            assert len(r.ctx.devices.list()) == 2 and r.local.lan_on() and not r.ctx.pairing.is_open
            # control: the phone's own token on its own door still works
            assert (await r.lan("GET", lite_web.STATUS_PATH, cookie=f"eve_dev={phone}")).status == 200

    run(go())


def test_cookie_flags_are_exact_per_door(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            reply = await r.mac("POST", lite_web.MAC_LOGIN_PATH, {"code": r.ctx.mac_login.issue()})
            mac = cookie_of(reply, web_auth.COOKIE_MAC)
            assert reply.cookies == [f"eve_mac={mac}; HttpOnly; SameSite=Strict; Path=/; Max-Age=34560000"]
            assert "Secure" not in reply.cookies[0]                           # plain http on loopback
            secret = (await r.open_pairing(mac))["links"][0]["url"].split("#p=", 1)[1]
            paired = await r.lan("POST", lite_web.PAIR_PATH, {"secret": secret})
            dev = cookie_of(paired, web_auth.COOKIE_DEV)
            assert paired.cookies == [f"eve_dev={dev}; HttpOnly; Secure; SameSite=Strict; Path=/; Max-Age=34560000"]
            assert dev != mac and len(dev) >= 43                              # 256-bit tokens, one per device
            again = await r.mac("GET", lite_web.STATUS_PATH, cookie=f"eve_mac={mac}")
            assert again.cookies == [f"eve_mac={mac}; HttpOnly; SameSite=Strict; Path=/; Max-Age=34560000"]
            stored = (tmp_path / "web-devices.json").read_text()
            assert mac not in stored and dev not in stored                    # only their hashes are kept

    run(go())


# ---- the OAuth callbacks -----------------------------------------------------------------------------------------

def test_the_openrouter_callback_is_taken_with_state_alone_and_refused_when_reused(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            mac = f"eve_mac={await r.login()}"
            start = await r.mac("GET", lite_web.OPENROUTER_START_PATH, cookie=mac)
            assert start.status == 302 and start.headers["location"].startswith(onboarding.OPENROUTER_AUTH_URL + "?")
            state = parse_qs(urlsplit(start.headers["location"]).query)["state"][0]
            callback = f"{lite_web.OPENROUTER_CALLBACK_PATH}?code=or-code-1&state={state}"
            # OpenRouter's redirect back: no cookie (SameSite=Strict is not sent cross-site), no Origin
            back = await r.mac("GET", callback)
            assert back.status == 303 and back.headers["location"] == "/"
            assert r.provider_calls == [onboarding.OPENROUTER_KEYS_URL]
            env = tmp_path / "env"
            assert envfile.parse_env_file(env.read_text())["OPENROUTER_API_KEY"] == CANARY_KEY
            assert stat.S_IMODE(env.stat().st_mode) == 0o600
            assert r.ctx.environ["OPENROUTER_API_KEY"] == CANARY_KEY
            await until(lambda: r.restarts == 1)                         # eve restarts into the key
            state_now = (await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)).json()
            assert state_now["openrouter"] is True and state_now["restarting"] is True and state_now["error"] == ""
            # the same callback again: refused, with no second exchange
            reused = await r.mac("GET", callback)
            assert reused.status == 400 and reused.headers["content-type"] == "text/html; charset=utf-8"
            assert b"already used or is out of date" in reused.body
            assert r.provider_calls == [onboarding.OPENROUTER_KEYS_URL]
            assert (await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)).json()["error"] == \
                onboarding.STATE_UNKNOWN
            # a state never issued, none at all, and a denial: each refused before anything leaves the Mac
            for query in ("code=c&state=never-issued-state", "code=c", ""):
                reply = await r.mac("GET", f"{lite_web.OPENROUTER_CALLBACK_PATH}?{query}")
                assert reply.status == 400, query
            fresh = await r.mac("GET", lite_web.OPENROUTER_START_PATH, cookie=mac)
            denied_state = parse_qs(urlsplit(fresh.headers["location"]).query)["state"][0]
            denied = await r.mac("GET", f"{lite_web.OPENROUTER_CALLBACK_PATH}?error=access_denied&state={denied_state}")
            assert denied.status == 400
            assert r.provider_calls == [onboarding.OPENROUTER_KEYS_URL] and r.restarts == 1
            # starting again clears the line the setup page shows
            await r.mac("GET", lite_web.OPENROUTER_START_PATH, cookie=mac)
            assert (await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)).json()["error"] == ""

    run(go())


def test_the_composio_callback_is_taken_with_state_alone_and_refused_when_reused(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            mac = f"eve_mac={await r.login()}"
            start = await r.mac("GET", lite_web.COMPOSIO_START_PATH, cookie=mac)
            assert start.status == 302 and start.headers["location"].startswith(onboarding.CONNECT_AUTHORIZE_URL)
            state = parse_qs(urlsplit(start.headers["location"]).query)["state"][0]
            callback = f"{lite_web.COMPOSIO_CALLBACK_PATH}?code=cc-code-1&state={state}&iss=whatever"
            back = await r.mac("GET", callback)
            assert back.status == 303 and back.headers["location"] == "/"
            token_file = tmp_path / "composio-connect.json"
            assert stat.S_IMODE(token_file.stat().st_mode) == 0o600
            assert json.loads(token_file.read_text())["access_token"] == CANARY_TOKEN + "-access"
            await until(lambda: r.restarts == 1)
            assert (await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)).json()["composio"] is True
            calls = list(r.provider_calls)
            assert calls == [onboarding.CONNECT_REGISTER_URL, onboarding.CONNECT_TOKEN_URL]
            reused = await r.mac("GET", callback)
            assert reused.status == 400 and r.provider_calls == calls
            cancelled = await r.mac("GET", f"{lite_web.COMPOSIO_CALLBACK_PATH}?error=access_denied")
            assert cancelled.status == 400 and r.provider_calls == calls and r.restarts == 1

    run(go())


def test_a_restart_that_fails_lets_the_page_go_on(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            def broken() -> None:
                raise RuntimeError("launchd said no")

            r.ctx.restart = broken
            mac = f"eve_mac={await r.login()}"
            start = await r.mac("GET", lite_web.OPENROUTER_START_PATH, cookie=mac)
            state = parse_qs(urlsplit(start.headers["location"]).query)["state"][0]
            assert (await r.mac("GET", f"{lite_web.OPENROUTER_CALLBACK_PATH}?code=c1&state={state}")).status == 303
            await until(lambda: not r.ctx.shared.restarting)             # not stuck waiting for ever
            assert (await r.mac("GET", lite_web.ONBOARD_STATE_PATH, cookie=mac)).json()["openrouter"] is True

    run(go())


# ---- what the JSON answers may carry ------------------------------------------------------------------------------

def test_the_key_scanner_catches_a_planted_canary() -> None:
    assert key_shaped(json.dumps({"credit": {"error": CANARY_KEY}}))
    assert key_shaped(json.dumps({"x": "sk-or-v1-" + "0a" * 32}))
    assert key_shaped("eyJhbGciOiJIUzI1NiJ9.e30.x")
    assert not key_shaped(json.dumps({"state": "READY", "openrouter": True, "credit": {"ok": True, "usd": 4.25}}))


def test_no_json_answer_carries_a_key_shaped_string(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    async def go() -> None:
        env = {"EVE_PROFILE": "lite", "OPENROUTER_API_KEY": CANARY_KEY}
        async with Rig(tmp_path, environ=env, apps=FakeBridge({"gmail": True})) as r:
            (tmp_path / "composio-connect.json").write_text(json.dumps({
                "version": 1, "access_token": CANARY_TOKEN, "refresh_token": CANARY_TOKEN, "client_id": "c"}))
            # everything that feeds an answer tries to carry a canary
            r.status = {"state": CANARY_KEY, "openrouter": True, "voice": True, "key": CANARY_KEY,
                        "detail": {"token": CANARY_TOKEN}}
            r.credit_answer = {"ok": True, "usd": CANARY_KEY, "funded": CANARY_KEY, "error": CANARY_KEY,
                               "key": CANARY_KEY}

            async def leaky_finish(code: str, state: str, *, error: str = "") -> str:
                raise onboarding.OnboardingError(f"OpenRouter said: bad key {CANARY_KEY}", 502)

            r.ctx.openrouter.finish = leaky_finish
            mac_token = await r.login()
            mac = f"eve_mac={mac_token}"
            failed = await r.mac("GET", f"{lite_web.OPENROUTER_CALLBACK_PATH}?code=c&state=s")
            assert failed.status == 502 and CANARY_KEY.encode() not in failed.body
            answers = {path: await r.mac("GET", path, cookie=mac) for path in (
                lite_web.ONBOARD_STATE_PATH, lite_web.STATUS_PATH, lite_web.HEALTH_PATH, lite_web.DEVICES_PATH,
                lite_web.APPS_PATH)}
            for path, reply in answers.items():
                assert reply.status == 200, path
                text = reply.body.decode()
                assert key_shaped(text) == [], (path, key_shaped(text))
                assert mac_token not in text and r.ctx.shared.nonce not in text, path
            assert answers[lite_web.ONBOARD_STATE_PATH].json()["error"] == lite_web.SIGNIN_BROKE
            status = answers[lite_web.STATUS_PATH].json()
            assert status["state"] == "READY" and status["credit"] == {"ok": True, "usd": None, "funded": None}
            assert set(status) == {"state", "openrouter", "openrouter_rejected", "composio", "voice", "lan",
                                   "local_network", "restarting", "credit"}
            # positive control: with the credit's guard taken away, the same request leaks, and the scan sees it
            monkeypatch.setattr(lite_web, "_credit_view", lambda raw: dict(raw))
            r.ctx.shared.credit = None
            leaked = (await r.mac("GET", lite_web.STATUS_PATH, cookie=mac)).body.decode()
            assert CANARY_KEY in leaked and key_shaped(leaked)

    run(go())


# ---- headers -------------------------------------------------------------------------------------------------------

def assert_secure(reply: Any) -> None:
    assert reply.headers["content-security-policy"] == lite_web.CSP
    assert "frame-ancestors 'none'" in reply.headers["content-security-policy"]
    assert "script-src 'self';" in reply.headers["content-security-policy"]
    assert reply.headers["x-content-type-options"] == "nosniff"
    assert reply.headers["referrer-policy"] == "no-referrer"
    assert reply.headers["permissions-policy"] == "microphone=(self), camera=(), geolocation=()"
    assert reply.headers["cross-origin-resource-policy"] == "same-origin"
    assert reply.headers["connection"] == "close"


def test_security_headers_are_on_every_answer(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), two_lines)
        async with Rig(tmp_path / "rig", link=link_door(tmp_path, inlet)) as r:
            token = await r.login()
            mac = f"eve_mac={token}"
            page = await r.mac("GET", "/")
            assert_secure(page)
            assert page.headers["content-type"] == "text/html; charset=utf-8"
            api = await r.mac("GET", lite_web.STATUS_PATH, cookie=mac)
            assert_secure(api)
            assert api.headers["cache-control"] == "no-store" and api.headers["content-type"] == "application/json"
            for reply in (await r.mac("GET", "/nothing"), await r.mac("GET", lite_web.STATUS_PATH),
                          await r.mac("GET", "/", host="evil.example"),
                          await r.mac("GET", lite_web.OPENROUTER_START_PATH, cookie=mac),
                          await r.mac("GET", f"{lite_web.OPENROUTER_CALLBACK_PATH}?code=x&state=y"),
                          await r.mac("GET", "/assets/eve-abc123.js")):
                assert_secure(reply)
                if reply.status != 200:
                    assert reply.headers["cache-control"] == "no-store"
            await r.open_pairing(token)
            for path in (lite_web.CA_PATH, lite_web.MOBILECONFIG_PATH, "/"):
                assert_secure(await r.lan("GET", path))
            # LinkChat's own answers get the two that cannot break a file it serves inline
            turn = await r.mac("POST", "/api/web/link/turn", {"text": "hi", "client_id": "h-1"}, cookie=mac)
            assert turn.headers["referrer-policy"] == "no-referrer" and turn.headers["x-content-type-options"] == "nosniff"
            assert turn.headers["cache-control"] == "no-store"

    run(go())


# ---- what reaches LinkChat ------------------------------------------------------------------------------------------

class SpyLink:
    """The real LinkChat with each request's arguments kept as it got them."""

    def __init__(self, link: Any) -> None:
        self.link = link
        self.uploads = link.uploads
        self.seen: list[dict[str, Any]] = []

    async def serve(self, method: str, path: str, query: str, headers: dict[str, str], reader: Any, writer: Any,
                    token_check: Any, **kw: Any) -> None:
        self.seen.append({"method": method, "path": path, "query": query, "headers": dict(headers),
                          "token_check": token_check})
        await self.link.serve(method, path, query, headers, reader, writer, token_check, **kw)

    def close(self) -> None:
        self.link.close()

    async def settle(self) -> None:
        await self.link.settle()


def test_a_turn_reaches_linkchat_with_origin_removed_and_the_nonce_set(tmp_path: Path) -> None:
    async def go() -> None:
        inlet = FakeInlet(store_at(tmp_path), two_lines)
        spy = SpyLink(link_door(tmp_path, inlet))
        async with Rig(tmp_path / "rig", link=spy) as r:
            token = await r.login()
            turn = await r.mac("POST", "/api/web/link/turn", {"text": "hello", "client_id": "nonce-1"},
                               cookie=f"eve_mac={token}", headers={"Accept": "text/event-stream"})
            assert turn.status == 200
            contents, done, _ = events(turn.body)
            assert contents == ["First line.", "\n\nSecond line."] and done     # LinkChat took the nonce
            seen = spy.seen[0]
            assert seen["method"] == "POST" and seen["path"] == "/api/link/turn"
            assert "origin" not in seen["headers"] and "cookie" not in seen["headers"]
            assert seen["headers"]["authorization"] == f"Bearer {r.ctx.shared.nonce}"
            assert seen["headers"]["accept"] == "text/event-stream"           # the rest as the page sent it
            assert seen["token_check"](r.ctx.shared.nonce) is True
            assert seen["token_check"](token) is False and seen["token_check"]("") is False
            assert r.ctx.shared.nonce.encode() not in turn.body
            # the same from a phone, over the LAN door
            phone = await r.pair(token)
            assert (await r.lan("GET", "/api/web/link/messages?wait=0", cookie=f"eve_dev={phone}")).status == 200
            seen = spy.seen[-1]
            assert seen["path"] == "/api/link/messages" and seen["query"] == "wait=0"
            assert "origin" not in seen["headers"] and "cookie" not in seen["headers"]
            assert seen["headers"]["authorization"] == f"Bearer {r.ctx.shared.nonce}"

    run(go())


@pytest.mark.parametrize("method, path", [
    ("GET", "/api/web/link/jobs"), ("POST", "/api/web/link/jobs"), ("POST", "/api/web/link/jobs/c1/go"),
    ("GET", "/api/web/link/apps"), ("GET", "/api/web/link/controls"), ("POST", "/api/web/link/controls/lights"),
    ("GET", "/api/web/link/controls/pika/stream"), ("GET", "/api/web/link/today"), ("GET", "/api/web/link/updates"),
    ("POST", "/api/web/link/updates/refresh"), ("POST", "/api/web/link/location"), ("GET", "/api/web/link/sessions"),
    ("POST", "/api/web/link/sessions/start"), ("GET", "/api/web/link/file/../../etc"),
    ("GET", "/api/web/link/file/ZZ"), ("GET", "/api/web/link/"), ("GET", "/api/web/link/messages/x"),
    ("GET", "/api/link/messages"), ("POST", "/api/link/turn"),
])
def test_linkchat_paths_outside_the_allowlist_are_404_at_the_door(tmp_path: Path, method: str, path: str) -> None:
    async def go() -> None:
        spy = SpyLink(link_door(tmp_path, FakeInlet(store_at(tmp_path))))
        async with Rig(tmp_path / "rig", link=spy) as r:
            token = await r.login()
            phone = await r.pair(token)
            assert (await r.mac(method, path, {}, cookie=f"eve_mac={token}")).status == 404
            assert (await r.lan(method, path, {}, cookie=f"eve_dev={phone}")).status == 404
            assert spy.seen == []                                             # LinkChat never saw it
            assert (await r.mac("GET", "/api/web/link/messages", cookie=f"eve_mac={token}")).status == 200
            assert len(spy.seen) == 1                                         # positive control

    run(go())


def test_wrong_methods_on_the_link_door_never_reach_linkchat(tmp_path: Path) -> None:
    async def go() -> None:
        spy = SpyLink(link_door(tmp_path, FakeInlet(store_at(tmp_path))))
        async with Rig(tmp_path / "rig", link=spy) as r:
            mac = f"eve_mac={await r.login()}"
            for method, path in (("GET", "/api/web/link/turn"), ("POST", "/api/web/link/messages"),
                                 ("GET", "/api/web/link/upload"), ("PUT", "/api/web/link/turn"),
                                 ("DELETE", lite_web.STATUS_PATH), ("POST", lite_web.STATUS_PATH)):
                reply = await r.mac(method, path, {}, cookie=mac)
                assert reply.status == 405, (method, path)
                assert reply.headers["allow"]
            assert spy.seen == []

    run(go())


# ---- requests that are not plain HTTP/1.1 ------------------------------------------------------------------------

@pytest.mark.parametrize("head, status", [
    (b"GET /healthz HTTP/1.1\r\nHost: {host}\r\nHost: {host}\r\n\r\n", 400),            # two Hosts
    (b"POST /onboard/composio/skip HTTP/1.1\r\nHost: {host}\r\nContent-Length: 2\r\nContent-Length: 3\r\n\r\n{{}}", 400),
    (b"GET /healthz HTTP/1.1\r\nHost: {host}\r\nX-A: a\r\n b\r\n\r\n", 400),             # an obs-fold
    (b"GET /healthz HTTP/1.1\r\nHost : {host}\r\n\r\n", 400),                           # a space before the colon
    (b"GET /healthz HTTP/1.1\r\nHost: {host}\r\nX-A: a\nb\r\n\r\n", 400),               # a bare LF
    (b"GET /healthz HTTP/1.1\r\nHost: {host}\r\nX-A: a\x00b\r\n\r\n", 400),             # a NUL
    (b"GET http://evil.example/healthz HTTP/1.1\r\nHost: {host}\r\n\r\n", 400),         # absolute form
    (b"GET /h\xc3\xa9 HTTP/1.1\r\nHost: {host}\r\n\r\n", 400),                         # not ASCII
    (b"GET /healthz HTTP/2.0\r\nHost: {host}\r\n\r\n", 400),
    (b"get /healthz HTTP/1.1\r\nHost: {host}\r\n\r\n", 400),
    (b"GET /healthz\r\nHost: {host}\r\n\r\n", 400),
    (b"GET /" + b"a" * 9000 + b" HTTP/1.1\r\nHost: {host}\r\n\r\n", 414),
    (b"GET /healthz HTTP/1.1\r\nHost: {host}\r\n" + b"X-Many: 1\r\n" * 70 + b"\r\n", 431),
    (b"GET /healthz HTTP/1.1\r\nHost: {host}\r\nX-Big: " + b"a" * 70000 + b"\r\n\r\n", 431),
])
def test_requests_that_are_not_plain_http_are_refused_before_routing(tmp_path: Path, head: bytes, status: int) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            raw = head.replace(b"{host}", f"localhost:{r.port}".encode()).replace(b"{{}}", b"{}")
            reply = await send_raw(r.port, raw)
            assert reply.startswith(f"HTTP/1.1 {status} ".encode()), reply[:80]
            assert not r.ctx.shared.saved.composio_skipped
            good = await send_raw(r.port, f"GET /healthz HTTP/1.1\r\nHost: localhost:{r.port}\r\n\r\n".encode())
            assert good.startswith(b"HTTP/1.1 200 ")                          # positive control

    run(go())


def test_a_client_that_never_finishes_its_head_is_let_go(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    async def go() -> None:
        monkeypatch.setattr(lite_web, "HEAD_SECS", 0.2)
        async with Rig(tmp_path) as r:
            reader, writer = await asyncio.open_connection("127.0.0.1", r.port)
            writer.write(b"GET /healthz HTTP/1.1\r\nHost: loc")
            await writer.drain()
            assert await asyncio.wait_for(reader.read(), 5) == b""           # closed, nothing said
            writer.close()

    run(go())


def test_too_many_connections_at_once_are_closed_at_once(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    async def go() -> None:
        monkeypatch.setattr(lite_web, "MAX_CONNECTIONS", 3)
        async with Rig(tmp_path) as r:
            idle = [await asyncio.open_connection("127.0.0.1", r.port) for _ in range(3)]
            await until(lambda: len(r.local._conns) == 3)                     # all three taken by the door
            reader, writer = await asyncio.open_connection("127.0.0.1", r.port)
            writer.write(f"GET /healthz HTTP/1.1\r\nHost: localhost:{r.port}\r\n\r\n".encode())
            try:
                closed = await asyncio.wait_for(reader.read(), 5)
            except ConnectionResetError:                                      # closed with the request unread
                closed = b""
            assert closed == b"" and r.local.refused == 1
            writer.close()
            for _, w in idle:
                w.close()
            await until(lambda: not r.local._conns)
            assert (await r.mac("GET", lite_web.HEALTH_PATH)).status == 200   # room again

    run(go())


def test_one_address_cannot_take_every_connection(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    async def go() -> None:
        monkeypatch.setattr(lite_web, "MAX_PER_PEER", 2)
        async with Rig(tmp_path) as r:
            idle = [await asyncio.open_connection("127.0.0.1", r.port) for _ in range(2)]
            await until(lambda: r.local._per_peer.get("127.0.0.1") == 2)
            reader, writer = await asyncio.open_connection("127.0.0.1", r.port)
            writer.write(f"GET /healthz HTTP/1.1\r\nHost: localhost:{r.port}\r\n\r\n".encode())
            try:
                closed = await asyncio.wait_for(reader.read(), 5)
            except ConnectionResetError:
                closed = b""
            assert closed == b"" and r.local.refused == 1
            writer.close()
            for _, w in idle:
                w.close()
            await until(lambda: not r.local._per_peer)
            assert (await r.mac("GET", lite_web.HEALTH_PATH)).status == 200   # its slots are free again
            assert r.local._per_peer == {}

    run(go())


def test_a_context_has_one_lan_door(tmp_path: Path) -> None:
    async def go() -> None:
        async with Rig(tmp_path) as r:
            await r.open_pairing(await r.login())
            with pytest.raises(RuntimeError, match="open already"):
                await lite_web.LanServer(r.ctx).start()
            assert r.local.lan_on()

    run(go())
