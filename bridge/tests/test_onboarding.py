"""eve lite's two browser sign-ins (onboarding.py): OpenRouter's PKCE key and Composio Connect's OAuth.

Every HTTP exchange goes through httpx.MockTransport, so the real request code runs and nothing leaves the Mac. The
Composio shapes are the ones spike S1 recorded on 2026-10-13 (tests/fixtures/composio_connect). Canaries are not
token-shaped (AGENTS rule 2), and each absence check has a planted positive control.
"""

from __future__ import annotations

import asyncio
import base64
import hashlib
import inspect
import json
import logging
import os
import re
import stat
import subprocess
from pathlib import Path
from typing import Any, Callable
from urllib.parse import parse_qs, urlsplit

import httpx
import pytest

from eve_bridge import onboarding, spend_sync, telegram
from eve_bridge.envfile import load_env_file
from eve_bridge.onboarding import (
    ComposioConnectLogin,
    OnboardingError,
    OpenRouterLogin,
    StateError,
    credit,
    save_openrouter_key,
)

FIXTURES = Path(__file__).parent / "fixtures" / "composio_connect"
REGISTRATION = json.loads((FIXTURES / "registration.json").read_text())
AUTHORIZE = json.loads((FIXTURES / "authorize_probe.json").read_text())
TOKEN_PROBE = json.loads((FIXTURES / "token_probe.json").read_text())
METADATA = json.loads((FIXTURES / "oauth_metadata.json").read_text())["documents"]

KEY = "CANARY-NOT-A-KEY-7f3c"
ACCESS = "CANARY-NOT-AN-ACCESS-TOKEN-7f3c"
REFRESH = "CANARY-NOT-A-REFRESH-TOKEN-7f3c"
ID_TOKEN = "CANARY-NOT-AN-ID-TOKEN-7f3c"
CLIENT_ID = REGISTRATION["response"]["client_id"]          # S1's placeholder, the real id's shape
OR_CALLBACK = "http://localhost:48780/onboard/openrouter/callback"
CP_CALLBACK = "http://localhost:48780/onboard/composio/callback"
CP_FORMS = [CP_CALLBACK, "http://127.0.0.1:48780/onboard/composio/callback"]
EXCHANGE = f"POST {onboarding.OPENROUTER_KEYS_URL}"
KEY_INFO = f"GET {spend_sync.OPENROUTER_KEY_URL}"
CREDITS = f"GET {spend_sync.OPENROUTER_CREDITS_URL}"
REGISTER = f"POST {onboarding.CONNECT_REGISTER_URL}"
TOKEN = f"POST {onboarding.CONNECT_TOKEN_URL}"
MANAGEMENT_ONLY = {"error": {"code": 403, "message": "Only management keys can perform this operation."}}
T0 = 1_800_000_000.0


ROOT = hasattr(os, "geteuid") and os.geteuid() == 0     # root writes into a 0500 folder anyway


class Clock:
    def __init__(self, now: float = T0) -> None:
        self.now = now

    def __call__(self) -> float:
        return self.now


class Provider:
    """A fake OpenRouter and Composio behind httpx.MockTransport. Routes are "METHOD url" -> an answer maker (sync
    or async) or an exception to raise; every request is kept."""

    def __init__(self, routes: dict[str, Any]) -> None:
        self.routes = dict(routes)
        self.seen: list[httpx.Request] = []
        self.http = httpx.AsyncClient(transport=httpx.MockTransport(self._answer))

    async def _answer(self, request: httpx.Request) -> httpx.Response:
        self.seen.append(request)
        route = self.routes.get(f"{request.method} {request.url.copy_with(query=None)}")
        if route is None:
            return httpx.Response(404, json={"error": "no such route in the fake"})
        if isinstance(route, type) and issubclass(route, httpx.HTTPError):
            raise route("the fake refused", request=request)
        answer = route(request)
        return await answer if inspect.isawaitable(answer) else answer

    def to(self, route: str) -> list[httpx.Request]:
        method, url = route.split(" ", 1)
        return [r for r in self.seen if r.method == method and str(r.url.copy_with(query=None)) == url]


def reply(status: int, body: Any = None, **kwargs: Any) -> Callable[[httpx.Request], httpx.Response]:
    return lambda _request: httpx.Response(status, json=body, **kwargs)


def registered(request: httpx.Request) -> httpx.Response:
    """Composio's 201 (registration.json): the request echoed, plus the client id and two defaults."""
    sent = json.loads(request.content)
    extra = {k: REGISTRATION["response"][k] for k in ("client_id", "client_id_issued_at", "application_type")}
    return httpx.Response(201, json={**sent, **extra})


def granted(**changes: Any) -> Callable[[httpx.Request], httpx.Response]:
    """The token endpoint's 200. Written with Python's own encoder, so a careless server's Infinity or NaN goes out
    as the bare word it would send (httpx's json= refuses them)."""
    answer = {"access_token": ACCESS, "refresh_token": REFRESH, "token_type": "Bearer", "expires_in": 300,
              "scope": onboarding.CONNECT_SCOPE, "id_token": ID_TOKEN}
    answer.update(changes)
    text = json.dumps({k: v for k, v in answer.items() if v is not None})
    return lambda _request: httpx.Response(200, text=text, headers={"content-type": "application/json"})


def run(coro: Any) -> Any:
    return asyncio.run(coro)


def query(url: str) -> dict[str, list[str]]:
    return parse_qs(urlsplit(url).query, keep_blank_values=True)


def state_of(url: str) -> str:
    return query(url)["state"][0]


def s256(verifier: str) -> str:
    return base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b"=").decode()


def unb64(text: str) -> bytes:
    return base64.urlsafe_b64decode(text + "=" * (-len(text) % 4))


def mode(path: Path) -> int:
    return stat.S_IMODE(os.stat(path).st_mode)


def jwt(claims: dict[str, Any]) -> str:
    """A JWT-shaped string built here, so the source holds nothing token-shaped. Unsigned: only ``exp`` is read."""
    def part(data: dict[str, Any]) -> str:
        return base64.urlsafe_b64encode(json.dumps(data).encode()).rstrip(b"=").decode()

    return ".".join([part({"alg": "none"}), part(claims), "unsigned"])


def openrouter(**routes: Any) -> tuple[OpenRouterLogin, Provider, Clock]:
    fake = Provider({EXCHANGE: reply(200, {"key": KEY, "user_id": "user-not-real"}), **routes})
    clock = Clock()
    return OpenRouterLogin(http=fake.http, clock=clock), fake, clock


def composio(tmp_path: Path, **routes: Any) -> tuple[ComposioConnectLogin, Provider, Clock]:
    fake = Provider({REGISTER: registered, TOKEN: granted(), **routes})
    clock = Clock()
    login = ComposioConnectLogin(tmp_path / "composio-connect.json", client_name="eve lite on test-mac",
                                 http=fake.http, clock=clock)
    return login, fake, clock


@pytest.fixture(autouse=True)
def _plain_logging():
    onboarding._warned.clear()
    yield
    telegram.unhide_tokens()


# ---- OpenRouter: the URL and PKCE -------------------------------------------------------------------------

def test_the_start_url_carries_the_callback_challenge_state_and_label() -> None:
    url = OpenRouterLogin().start(OR_CALLBACK, "eve lite on test-mac")
    parts = urlsplit(url)
    assert (parts.scheme, parts.netloc, parts.path) == ("https", "openrouter.ai", "/auth")
    q = query(url)
    assert sorted(q) == ["callback_url", "code_challenge", "code_challenge_method", "key_label", "state"]
    assert q["callback_url"] == [OR_CALLBACK] and q["code_challenge_method"] == ["S256"]
    assert q["key_label"] == ["eve lite on test-mac"]
    assert "key_label=eve%20lite%20on%20test-mac" in parts.query          # %20, as the spec's URL writes it
    assert len(unb64(q["state"][0])) == 16                                   # 128 bits
    assert re.fullmatch(r"[A-Za-z0-9_-]{43}", q["code_challenge"][0])       # 32 bytes, unpadded base64url


def test_the_challenge_is_the_unpadded_base64url_sha256_of_the_verifier_sent() -> None:
    login, fake, _ = openrouter()
    url = login.start(OR_CALLBACK, "x")
    assert run(login.finish("code-1", state_of(url))) == KEY
    verifier = json.loads(fake.seen[0].content)["code_verifier"]
    assert 43 <= len(verifier) <= 128 and re.fullmatch(r"[A-Za-z0-9._~-]+", verifier)      # RFC 7636 4.1
    challenge = query(url)["code_challenge"][0]
    assert "=" not in challenge and challenge == s256(verifier)
    assert unb64(challenge) == hashlib.sha256(verifier.encode()).digest()                 # decoded, the digest


def test_each_start_gets_its_own_state_and_verifier() -> None:
    login, fake, _ = openrouter()
    urls = [login.start(OR_CALLBACK, "x") for _ in range(3)]
    assert len({state_of(u) for u in urls}) == 3 and len({query(u)["code_challenge"][0] for u in urls}) == 3


def test_the_exchange_body_is_exactly_code_verifier_and_method() -> None:
    login, fake, _ = openrouter()
    run(login.finish("code-1", state_of(login.start(OR_CALLBACK, "x"))))
    [request] = fake.seen
    assert request.method == "POST" and str(request.url) == "https://openrouter.ai/api/v1/auth/keys"
    assert request.headers["content-type"] == "application/json"
    assert "authorization" not in request.headers
    body = json.loads(request.content)
    assert set(body) == {"code", "code_verifier", "code_challenge_method"}
    assert body["code"] == "code-1" and body["code_challenge_method"] == "S256"


# ---- OpenRouter: the state --------------------------------------------------------------------------------

@pytest.mark.parametrize("state", ["", None, "not-the-state", ["a", "list"]])
def test_a_wrong_or_missing_state_is_400_and_nothing_is_sent(state: Any) -> None:
    login, fake, _ = openrouter()
    good = state_of(login.start(OR_CALLBACK, "x"))
    with pytest.raises(StateError) as e:
        run(login.finish("code-1", state))
    assert e.value.status == 400 and str(e.value) in (onboarding.STATE_MISSING, onboarding.STATE_UNKNOWN)
    assert fake.seen == []
    assert run(login.finish("code-1", good)) == KEY          # control: the real state still works


def test_a_reused_state_is_400_and_the_second_exchange_never_happens() -> None:
    login, fake, _ = openrouter()
    state = state_of(login.start(OR_CALLBACK, "x"))
    assert run(login.finish("code-1", state)) == KEY
    with pytest.raises(StateError) as e:
        run(login.finish("code-1", state))
    assert e.value.status == 400 and str(e.value) == onboarding.STATE_UNKNOWN
    assert len(fake.seen) == 1


def test_a_failed_exchange_uses_the_state_up_too() -> None:
    login, fake, _ = openrouter(**{EXCHANGE: reply(500, {"error": {"code": 500, "message": "oops"}})})
    state = state_of(login.start(OR_CALLBACK, "x"))
    with pytest.raises(OnboardingError):
        run(login.finish("code-1", state))
    with pytest.raises(StateError):
        run(login.finish("code-1", state))
    assert len(fake.seen) == 1


def test_a_state_older_than_ten_minutes_is_400_and_nothing_is_sent() -> None:
    login, fake, clock = openrouter()
    first, second = (state_of(login.start(OR_CALLBACK, "x")) for _ in range(2))
    clock.now += onboarding.STATE_TTL_SECS - 1
    assert run(login.finish("code-1", first)) == KEY             # control: inside the window it works
    clock.now += 2
    with pytest.raises(StateError) as e:
        run(login.finish("code-1", second))
    assert e.value.status == 400 and str(e.value) == onboarding.STATE_EXPIRED
    assert len(fake.seen) == 1


def test_at_most_max_pending_sign_ins_wait_and_the_oldest_goes_first() -> None:
    login, fake, _ = openrouter()
    states = [state_of(login.start(OR_CALLBACK, "x")) for _ in range(onboarding.MAX_PENDING + 1)]
    with pytest.raises(StateError):
        run(login.finish("code-1", states[0]))
    assert fake.seen == []
    assert run(login.finish("code-1", states[-1])) == KEY
    assert run(login.finish("code-1", states[1])) == KEY


def test_one_sign_ins_state_is_no_good_for_the_other(tmp_path: Path) -> None:
    login, fake, _ = openrouter()
    connect, cfake, _ = composio(tmp_path)
    with pytest.raises(StateError):
        run(connect.finish("code-1", state_of(login.start(OR_CALLBACK, "x"))))
    with pytest.raises(StateError):
        run(login.finish("code-1", state_of(run(connect.start(CP_CALLBACK)))))
    assert fake.seen == [] and cfake.to(TOKEN) == []


# ---- OpenRouter: failures are plain lines -----------------------------------------------------------------

@pytest.mark.parametrize("message, line", [
    ("Invalid code or code_verifier", onboarding.OPENROUTER_REFUSED),
    ("Authorization code expired", onboarding.OPENROUTER_CODE_EXPIRED),
])
def test_a_403_and_an_expired_code_each_give_their_own_plain_line(message: str, line: str) -> None:
    login, _, _ = openrouter(**{EXCHANGE: reply(403, {"error": {"code": 403, "message": message}})})
    with pytest.raises(OnboardingError) as e:
        run(login.finish("code-1", state_of(login.start(OR_CALLBACK, "x"))))
    assert str(e.value) == line and not isinstance(e.value, StateError)
    assert "\n" not in line and message not in line and "{" not in line
    assert onboarding.OPENROUTER_REFUSED != onboarding.OPENROUTER_CODE_EXPIRED


@pytest.mark.parametrize("route, line", [
    (reply(500, {"error": {"code": 500, "message": "Internal Server Error"}}),
     onboarding.OPENROUTER_FAILED.format(status=500)),
    (reply(400, {"error": {"code": 400, "message": "Invalid code_challenge_method"}}), onboarding.OPENROUTER_REFUSED),
    (reply(200, {"user_id": "user-not-real"}), onboarding.OPENROUTER_NO_KEY),
    (reply(200, {"key": "two words"}), onboarding.OPENROUTER_NO_KEY),
    (lambda _r: httpx.Response(200, text="<html>not json</html>"), onboarding.OPENROUTER_NO_KEY),
    (httpx.ConnectError, onboarding.OPENROUTER_UNREACHABLE),
    (httpx.ReadTimeout, onboarding.OPENROUTER_UNREACHABLE),
])
def test_every_other_failure_is_a_plain_line(route: Any, line: str) -> None:
    login, _, _ = openrouter(**{EXCHANGE: route})
    with pytest.raises(OnboardingError) as e:
        run(login.finish("code-1", state_of(login.start(OR_CALLBACK, "x"))))
    assert str(e.value) == line and e.value.status == 502
    assert e.value.__cause__ is None and (e.value.__context__ is None or e.value.__suppress_context__)


def test_a_key_with_stray_whitespace_around_it_is_trimmed() -> None:
    login, _, _ = openrouter(**{EXCHANGE: reply(200, {"key": f"  {KEY}\n"})})
    assert run(login.finish("code-1", state_of(login.start(OR_CALLBACK, "x")))) == KEY


@pytest.mark.parametrize("code, error", [("", ""), (None, ""), ("code-1", "access_denied"),
                                         ("x" * (onboarding.MAX_CODE_CHARS + 1), "")])
def test_a_denied_or_codeless_callback_sends_nothing(code: Any, error: str) -> None:
    login, fake, _ = openrouter()
    with pytest.raises(OnboardingError) as e:
        run(login.finish(code, state_of(login.start(OR_CALLBACK, "x")), error=error))
    assert e.value.status == 400 and str(e.value) == onboarding.OPENROUTER_NOT_AUTHORIZED
    assert fake.seen == []


# ---- OpenRouter: keeping the key --------------------------------------------------------------------------

def test_success_writes_the_key_0600_and_keeps_the_other_lines(tmp_path: Path) -> None:
    f = tmp_path / "env"
    f.write_text("# mine\nEVE_PROFILE=lite\nOTHER=1\n")
    os.chmod(f, 0o644)
    login, _, _ = openrouter()
    key = run(login.finish("code-1", state_of(login.start(OR_CALLBACK, "x"))))
    env: dict[str, str] = {}
    save_openrouter_key(key, path=f, environ=env)
    assert f.read_text() == f"# mine\nEVE_PROFILE=lite\nOTHER=1\nOPENROUTER_API_KEY={KEY}\n"
    assert mode(f) == 0o600
    assert env == {"OPENROUTER_API_KEY": KEY}


def test_a_new_key_replaces_the_old_one(tmp_path: Path) -> None:
    f = tmp_path / "env"
    f.write_text("EVE_PROFILE=lite\nOPENROUTER_API_KEY=old-value-here\n")
    env = {"OPENROUTER_API_KEY": "old-value-here"}
    save_openrouter_key(KEY, path=f, environ=env)
    assert f.read_text() == f"EVE_PROFILE=lite\nOPENROUTER_API_KEY={KEY}\n" and env["OPENROUTER_API_KEY"] == KEY


def test_without_a_path_the_key_goes_to_the_env_file_the_daemon_reads(tmp_path: Path,
                                                                       monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    env: dict[str, str] = {}
    save_openrouter_key(KEY, environ=env)
    f = tmp_path / ".config" / "eve-bridge" / "env"
    assert f.read_text() == f"OPENROUTER_API_KEY={KEY}\n" and mode(f) == 0o600
    loaded: dict[str, str] = {}
    load_env_file(environ=loaded)
    assert loaded == {"OPENROUTER_API_KEY": KEY}
    shared = tmp_path / "shared-env"
    save_openrouter_key(KEY, environ={"EVE_ENV_FILE": str(shared)})        # EVE_ENV_FILE wins, as for the loader
    assert shared.read_text() == f"OPENROUTER_API_KEY={KEY}\n"


@pytest.mark.parametrize("bad", ["", "short", "two words here", f"{KEY}\nEVE_PROFILE=full", None, 42])
def test_save_refuses_what_is_not_a_key_and_writes_nothing(tmp_path: Path, bad: Any) -> None:
    f = tmp_path / "env"
    env: dict[str, str] = {}
    with pytest.raises(ValueError) as e:
        save_openrouter_key(bad, path=f, environ=env)
    assert not f.exists() and env == {}
    assert KEY not in str(e.value)


@pytest.mark.skipif(ROOT, reason="root ignores folder permissions")
def test_a_save_that_cannot_write_is_a_plain_line(tmp_path: Path) -> None:
    folder = tmp_path / "locked"
    folder.mkdir()
    os.chmod(folder, 0o500)
    env: dict[str, str] = {}
    try:
        with pytest.raises(OnboardingError) as e:
            save_openrouter_key(KEY, path=folder / "env", environ=env)
    finally:
        os.chmod(folder, 0o700)
    assert e.value.status == 500 and KEY not in str(e.value) and env == {}
    assert list(folder.iterdir()) == []


def test_the_key_is_hidden_from_every_log_record_the_moment_it_arrives() -> None:
    login, _, _ = openrouter()
    make = logging.getLogRecordFactory()
    assert KEY in make("httpx", logging.INFO, __file__, 1, f"before {KEY}", None, None).getMessage()   # control
    run(login.finish("code-1", state_of(login.start(OR_CALLBACK, "x"))))
    after = logging.getLogRecordFactory()("httpx", logging.INFO, __file__, 1, f"after {KEY}", None, None)
    assert KEY not in after.getMessage() and "<token>" in after.getMessage()


# ---- OpenRouter: credit -----------------------------------------------------------------------------------

def key_info(**changes: Any) -> dict[str, Any]:
    info = {"label": "eve lite on test-mac", "limit": None, "limit_remaining": None, "limit_reset": None,
            "is_free_tier": False, "usage": 0.5, "usage_daily": 0.5}
    info.update(changes)
    return {"data": info}


def account(bought: float, used: float) -> dict[str, Any]:
    return {"data": {"total_credits": bought, "total_usage": used}}


def test_credit_is_the_account_balance_read_with_the_key_as_a_bearer() -> None:
    fake = Provider({KEY_INFO: reply(200, key_info()), CREDITS: reply(200, account(10.0, 2.5))})
    assert run(credit(KEY, http=fake.http)) == {"ok": True, "usd": 7.5, "funded": True}
    assert sorted(str(r.url) for r in fake.seen) == sorted([spend_sync.OPENROUTER_KEY_URL,
                                                            spend_sync.OPENROUTER_CREDITS_URL])
    for request in fake.seen:
        assert request.method == "GET" and request.headers["authorization"] == f"Bearer {KEY}"
        assert request.content == b"" and KEY not in str(request.url)


@pytest.mark.parametrize("info, balance, shown", [
    (key_info(), (200, account(5.0, 5.0)), {"ok": True, "usd": 0.0, "funded": False}),
    (key_info(), (200, account(5.0, 5.5)), {"ok": True, "usd": 0.0, "funded": False}),        # owing reads $0
    (key_info(), (200, account(1.0, 0.996)), {"ok": True, "usd": 0.0, "funded": False}),      # under a cent
    (key_info(limit=2.0, limit_remaining=1.25), (200, account(10.0, 2.5)), {"ok": True, "usd": 1.25, "funded": True}),
    # /credits needs a management key (OpenRouter's docs), which an OAuth key is not:
    (key_info(is_free_tier=True), (403, MANAGEMENT_ONLY), {"ok": True, "usd": None, "funded": False}),
    (key_info(), (403, MANAGEMENT_ONLY), {"ok": True, "usd": None, "funded": None}),
    (key_info(limit=3.0, limit_remaining=3.0), (403, MANAGEMENT_ONLY), {"ok": True, "usd": 3.0, "funded": True}),
    (key_info(), (200, {"data": {"total_credits": "lots"}}), {"ok": True, "usd": None, "funded": None}),
])
def test_credit_says_what_the_key_can_spend(info: dict, balance: tuple, shown: dict) -> None:
    fake = Provider({KEY_INFO: reply(200, info), CREDITS: reply(*balance)})
    assert run(credit(KEY, http=fake.http)) == shown


@pytest.mark.parametrize("route, why", [
    (reply(401, {"error": {"code": 401, "message": "User not found."}}), "HTTP 401"),
    (lambda _r: httpx.Response(200, text="<html>"), "unreadable reply"),
    (reply(200, {"data": "nope"}), "unreadable reply"),
    (httpx.ConnectError, "unreachable"),
    (httpx.ConnectTimeout, "timed out"),
])
def test_credit_never_raises_and_says_why(route: Any, why: str) -> None:
    fake = Provider({KEY_INFO: route, CREDITS: reply(200, account(10.0, 2.5))})
    expected = {"ok": False, "usd": None, "funded": None, "error": why}
    if why == "HTTP 401":
        expected["rejected"] = True                   # a 401 is the one error that means "sign the key in again"
    assert run(credit(KEY, http=fake.http)) == expected


HUGE = "1" + "0" * 400                       # a JSON integer no float can hold


@pytest.mark.parametrize("info, balance, shown", [
    ('{"data": {"limit_remaining": null, "is_free_tier": false}}',
     '{"data": {"total_credits": Infinity, "total_usage": 1.0}}', {"ok": True, "usd": None, "funded": None}),
    ('{"data": {"limit_remaining": NaN, "is_free_tier": true}}',
     '{"data": {"total_credits": 5.0, "total_usage": NaN}}', {"ok": True, "usd": None, "funded": False}),
    ('{"data": {"limit_remaining": ' + HUGE + ', "is_free_tier": false}}',
     '{"data": {"total_credits": ' + HUGE + ', "total_usage": 1.0}}', {"ok": True, "usd": None, "funded": None}),
    ('{"data": {"limit_remaining": 2.0, "is_free_tier": false}}',
     '{"data": {"total_credits": Infinity, "total_usage": 1.0}}', {"ok": True, "usd": 2.0, "funded": True}),
])
def test_credit_reads_a_figure_that_is_not_a_finite_number_as_unknown(info: str, balance: str, shown: dict) -> None:
    def raw(text: str) -> Callable[[httpx.Request], httpx.Response]:
        return lambda _r: httpx.Response(200, text=text, headers={"content-type": "application/json"})

    fake = Provider({KEY_INFO: raw(info), CREDITS: raw(balance)})
    result = run(credit(KEY, http=fake.http))
    assert result == shown
    json.dumps(result, allow_nan=False)                         # the page's JSON stays strict


def test_credit_never_raises_even_on_a_closed_client() -> None:
    fake = Provider({KEY_INFO: reply(200, key_info()), CREDITS: reply(200, account(10.0, 2.5))})
    assert run(credit(KEY, http=fake.http))["ok"] is True                  # control: the client works
    run(fake.http.aclose())
    assert run(credit(KEY, http=fake.http)) == {"ok": False, "usd": None, "funded": None, "error": "unreachable"}


def test_credit_hides_the_key_it_is_given() -> None:
    make = logging.getLogRecordFactory()
    assert KEY in make("httpx", logging.INFO, __file__, 1, f"before {KEY}", None, None).getMessage()   # control
    fake = Provider({KEY_INFO: reply(200, key_info()), CREDITS: reply(200, account(10.0, 2.5))})
    run(credit(KEY, http=fake.http))                              # a fresh process: the key came from the env file
    after = logging.getLogRecordFactory()("httpx", logging.INFO, __file__, 1, f"after {KEY}", None, None)
    assert KEY not in after.getMessage()


def test_credit_without_a_key_sends_nothing() -> None:
    fake = Provider({})
    assert run(credit("", http=fake.http)) == {"ok": False, "usd": None, "funded": None, "error": "no key"}
    assert fake.seen == []


# ---- Composio Connect: registration and the authorize URL -------------------------------------------------

def test_the_pinned_endpoints_are_the_ones_composio_publishes() -> None:
    server = METADATA["https://connect.composio.dev/.well-known/oauth-authorization-server"]["body"]
    resource = METADATA["https://connect.composio.dev/.well-known/oauth-protected-resource"]["body"]
    assert onboarding.CONNECT_ISSUER == server["issuer"]
    assert onboarding.CONNECT_AUTHORIZE_URL == server["authorization_endpoint"]
    assert onboarding.CONNECT_TOKEN_URL == server["token_endpoint"]
    assert onboarding.CONNECT_REGISTER_URL == server["registration_endpoint"]
    assert onboarding.CONNECT_RESOURCE == resource["resource"]
    assert onboarding.CONNECT_SCOPE.split() == server["scopes_supported"]
    assert server["code_challenge_methods_supported"] == ["S256"]
    assert server["token_endpoint_auth_methods_supported"] == ["none"]


def test_the_registration_body_is_the_one_s1_verified(tmp_path: Path) -> None:
    login, fake, _ = composio(tmp_path)
    run(login.start(CP_CALLBACK))
    [request] = fake.to(REGISTER)
    assert request.headers["content-type"] == "application/json" and "authorization" not in request.headers
    body = json.loads(request.content)
    assert body == {**REGISTRATION["request"], "client_name": "eve lite on test-mac"}
    assert body["token_endpoint_auth_method"] == "none"
    assert body["redirect_uris"] == CP_FORMS
    for uri in body["redirect_uris"]:
        parts = urlsplit(uri)
        assert parts.scheme == "http" and parts.hostname in ("localhost", "127.0.0.1") and parts.port == 48780


def test_the_authorize_url_has_the_resource_the_scope_and_s256(tmp_path: Path) -> None:
    login, fake, _ = composio(tmp_path)
    url = run(login.start(CP_CALLBACK))
    parts = urlsplit(url)
    assert f"{parts.scheme}://{parts.netloc}{parts.path}" == "https://connect.composio.dev/oauth/authorize"
    q = query(url)
    assert set(q) == set(AUTHORIZE["parameters"])                          # exactly the keys S1 sent
    assert q["resource"] == ["https://connect.composio.dev/mcp"]
    assert q["scope"] == ["openid profile email offline_access"]
    assert q["code_challenge_method"] == ["S256"] and q["response_type"] == ["code"]
    assert q["client_id"] == [CLIENT_ID] and q["redirect_uri"] == [CP_CALLBACK]
    assert re.fullmatch(r"[A-Za-z0-9_-]{43}", q["code_challenge"][0]) and len(unb64(q["state"][0])) == 16
    assert "scope=openid%20profile%20email%20offline_access" in parts.query      # S1's URL
    assert "resource=https%3A%2F%2Fconnect.composio.dev%2Fmcp" in parts.query
    assert not login.token_path.exists()                                   # nothing is written before a token


@pytest.mark.parametrize("callback, told", [
    ("http://127.0.0.1:48783/onboard/composio/callback", "http://127.0.0.1:48783/onboard/composio/callback"),
    ("http://[::1]:48783/onboard/composio/callback", "http://localhost:48783/onboard/composio/callback"),
    ("http://LOCALHOST:48783/onboard/composio/callback", "http://localhost:48783/onboard/composio/callback"),
])
def test_composio_is_told_the_pages_own_loopback_form(tmp_path: Path, callback: str, told: str) -> None:
    login, fake, _ = composio(tmp_path)
    assert query(run(login.start(callback)))["redirect_uri"] == [told]
    assert json.loads(fake.to(REGISTER)[0].content)["redirect_uris"] == [
        "http://localhost:48783/onboard/composio/callback", "http://127.0.0.1:48783/onboard/composio/callback"]


@pytest.mark.parametrize("host", ["localhost", "127.0.0.1", "[::1]"])
def test_openrouter_is_always_told_the_localhost_form_it_documents(host: str) -> None:
    url = OpenRouterLogin().start(f"http://{host}:48783/onboard/openrouter/callback", "x")
    assert query(url)["callback_url"] == ["http://localhost:48783/onboard/openrouter/callback"]


@pytest.mark.parametrize("bad", [
    "http://192.168.1.5:48781/onboard/composio/callback", "https://localhost:48780/onboard/composio/callback",
    "http://localhost/onboard/composio/callback", "http://eve.example:48780/onboard/composio/callback",
    "http://localhost:48780/onboard/composio/callback?next=x", "http://someone@localhost:48780/cb",
    "http://localhost:48780/cb#frag", "http://localhost:99999/cb", "localhost:48780/cb", "", None,
])
def test_a_callback_off_this_mac_is_refused_before_any_request(tmp_path: Path, bad: Any) -> None:
    login, fake, _ = composio(tmp_path)
    with pytest.raises(ValueError):
        run(login.start(bad))
    with pytest.raises(ValueError):
        OpenRouterLogin(http=fake.http).start(bad, "x")
    assert fake.seen == []


def test_a_second_start_reuses_the_registration(tmp_path: Path) -> None:
    login, fake, _ = composio(tmp_path)
    first, second = run(login.start(CP_CALLBACK)), run(login.start("http://127.0.0.1:48780/onboard/composio/callback"))
    assert len(fake.to(REGISTER)) == 1
    assert query(first)["client_id"] == query(second)["client_id"] == [CLIENT_ID]
    assert state_of(first) != state_of(second)


class NoLock:
    async def __aenter__(self) -> None:
        return None

    async def __aexit__(self, *_exc: Any) -> bool:
        return False


@pytest.mark.parametrize("locked", [True, False])
def test_two_quick_clicks_register_once(tmp_path: Path, locked: bool) -> None:
    async def slow(request: httpx.Request) -> httpx.Response:
        await asyncio.sleep(0.01)
        return registered(request)

    login, fake, _ = composio(tmp_path, **{REGISTER: slow})
    if not locked:
        login._lock = NoLock()                    # control: without the lock both clicks register

    async def both() -> None:
        await asyncio.gather(login.start(CP_CALLBACK), login.start(CP_CALLBACK))

    run(both())
    assert len(fake.to(REGISTER)) == (1 if locked else 2)


def test_a_new_process_reuses_the_client_kept_in_the_token_file(tmp_path: Path) -> None:
    path = tmp_path / "composio-connect.json"
    path.write_text(json.dumps({"version": 1, "issuer": onboarding.CONNECT_ISSUER, "client_id": "client-kept-in-file",
                                "redirect_uris": CP_FORMS, "access_token": "", "refresh_token": "",
                                "expires_at": 0, "scope": onboarding.CONNECT_SCOPE}))
    login, fake, _ = composio(tmp_path)
    assert query(run(login.start(CP_CALLBACK)))["client_id"] == ["client-kept-in-file"]
    assert fake.to(REGISTER) == []
    other_port = "http://localhost:48784/onboard/composio/callback"                 # control: not listed there
    assert query(run(login.start(other_port)))["client_id"] == [CLIENT_ID]
    assert len(fake.to(REGISTER)) == 1


@pytest.mark.parametrize("stored", [
    {"version": 2, "client_id": "client-kept-in-file", "redirect_uris": CP_FORMS},          # a format eve does not know
    {"client_id": "client-kept-in-file", "redirect_uris": CP_FORMS},                        # no version at all
    {"version": 1, "client_id": "client kept in file", "redirect_uris": CP_FORMS},          # not an id
    {"version": 1, "client_id": "client-kept-in-file", "redirect_uris": CP_CALLBACK},       # not a list
    ["not", "an", "object"],
])
def test_a_token_file_eve_cannot_vouch_for_is_not_reused(tmp_path: Path, stored: Any) -> None:
    (tmp_path / "composio-connect.json").write_text(json.dumps(stored))
    login, fake, _ = composio(tmp_path)
    assert query(run(login.start(CP_CALLBACK)))["client_id"] == [CLIENT_ID]
    assert len(fake.to(REGISTER)) == 1


@pytest.mark.parametrize("route, line", [
    (reply(400, {"error": "invalid_redirect_uri"}), onboarding.COMPOSIO_NOT_REGISTERED.format(status=400)),
    (reply(201, {"redirect_uris": CP_FORMS}), onboarding.COMPOSIO_NOT_REGISTERED.format(status=201)),
    (lambda r: httpx.Response(201, json={**json.loads(r.content), "client_id": CLIENT_ID,
                                         "redirect_uris": ["http://localhost:1/elsewhere"]}),
     onboarding.COMPOSIO_NO_CALLBACK),
    (httpx.ConnectError, onboarding.COMPOSIO_UNREACHABLE),
])
def test_a_registration_that_fails_is_a_plain_line_and_starts_nothing(tmp_path: Path, route: Any,
                                                                       line: str) -> None:
    login, _, _ = composio(tmp_path, **{REGISTER: route})
    with pytest.raises(OnboardingError) as e:
        run(login.start(CP_CALLBACK))
    assert str(e.value) == line and e.value.status == 502
    assert len(login._states) == 0 and not login.token_path.exists()


# ---- Composio Connect: the callback -----------------------------------------------------------------------

@pytest.mark.parametrize("state", ["", None, "not-the-state"])
def test_a_wrong_or_missing_composio_state_is_400_and_nothing_is_sent(tmp_path: Path, state: Any) -> None:
    login, fake, _ = composio(tmp_path)
    run(login.start(CP_CALLBACK))
    with pytest.raises(StateError) as e:
        run(login.finish("code-1", state))
    assert e.value.status == 400 and fake.to(TOKEN) == [] and not login.token_path.exists()


def test_a_reused_or_expired_composio_state_is_400_and_nothing_is_sent(tmp_path: Path) -> None:
    login, fake, clock = composio(tmp_path)
    first, second = run(login.start(CP_CALLBACK)), run(login.start(CP_CALLBACK))
    run(login.finish("code-1", state_of(first)))
    with pytest.raises(StateError):
        run(login.finish("code-1", state_of(first)))
    clock.now += onboarding.STATE_TTL_SECS + 1
    with pytest.raises(StateError) as e:
        run(login.finish("code-1", state_of(second)))
    assert str(e.value) == onboarding.STATE_EXPIRED
    assert len(fake.to(TOKEN)) == 1


def test_the_token_exchange_is_the_public_client_form_s1_verified(tmp_path: Path) -> None:
    login, fake, _ = composio(tmp_path)
    url = run(login.start(CP_CALLBACK))
    run(login.finish("code-1", state_of(url)))
    [request] = fake.to(TOKEN)
    assert request.headers["content-type"] == "application/x-www-form-urlencoded"
    assert "authorization" not in request.headers                          # no secret, no Basic header
    form = parse_qs(request.content.decode(), keep_blank_values=True)
    assert sorted(form) == sorted(TOKEN_PROBE["cases"][0]["request_keys"])
    assert form["grant_type"] == ["authorization_code"] and form["code"] == ["code-1"]
    assert form["redirect_uri"] == [CP_CALLBACK] and form["client_id"] == [CLIENT_ID]
    assert form["resource"] == ["https://connect.composio.dev/mcp"]
    assert query(url)["code_challenge"] == [s256(form["code_verifier"][0])]


def test_success_writes_a_0600_token_file_with_exactly_the_contract_keys(tmp_path: Path) -> None:
    login, _, clock = composio(tmp_path)
    run(login.finish("code-1", state_of(run(login.start(CP_CALLBACK)))))
    assert mode(login.token_path) == 0o600
    assert json.loads(login.token_path.read_text()) == {
        "version": 1, "issuer": "https://connect.composio.dev", "client_id": CLIENT_ID, "redirect_uris": CP_FORMS,
        "access_token": ACCESS, "refresh_token": REFRESH, "expires_at": clock.now + 300,
        "scope": "openid profile email offline_access"}
    assert ID_TOKEN not in login.token_path.read_text()                    # the id token (an email) is not kept


@pytest.mark.parametrize("changes, expires_in", [
    ({"expires_in": "600"}, 600.0),
    ({"expires_in": 299.5}, 299.5),
    ({"expires_in": None, "access_token": jwt({"exp": T0 + 1234, "sub": "someone"})}, 1234.0),
    ({"expires_in": None, "access_token": jwt({"exp": T0 - 5})}, onboarding.DEFAULT_TOKEN_SECS),
    ({"expires_in": None, "access_token": jwt({"exp": float("inf")})}, onboarding.DEFAULT_TOKEN_SECS),
    ({"expires_in": None, "access_token": jwt({"exp": 10**400})}, onboarding.DEFAULT_TOKEN_SECS),     # no float holds it
    ({"expires_in": None, "access_token": jwt({"exp": T0 + 10**12})}, onboarding.DEFAULT_TOKEN_SECS),  # 31,000 years
    ({"expires_in": 10**400}, onboarding.DEFAULT_TOKEN_SECS),
    ({"expires_in": None, "access_token": jwt({"exp": "soon"})}, onboarding.DEFAULT_TOKEN_SECS),
    ({"expires_in": None}, onboarding.DEFAULT_TOKEN_SECS),
    ({"expires_in": 0}, onboarding.DEFAULT_TOKEN_SECS),
    ({"expires_in": -60}, onboarding.DEFAULT_TOKEN_SECS),
    ({"expires_in": True}, onboarding.DEFAULT_TOKEN_SECS),
    ({"expires_in": "²"}, onboarding.DEFAULT_TOKEN_SECS),                  # isdigit() says yes, int() says no
    ({"expires_in": "soon"}, onboarding.DEFAULT_TOKEN_SECS),
    ({"expires_in": float("inf")}, onboarding.DEFAULT_TOKEN_SECS),
    ({"expires_in": float("nan")}, onboarding.DEFAULT_TOKEN_SECS),
    ({"expires_in": 10**12}, onboarding.DEFAULT_TOKEN_SECS),
])
def test_the_expiry_comes_from_expires_in_then_the_tokens_own_exp(tmp_path: Path, changes: dict,
                                                                  expires_in: float) -> None:
    login, _, clock = composio(tmp_path, **{TOKEN: granted(**changes)})
    run(login.finish("code-1", state_of(run(login.start(CP_CALLBACK)))))

    def not_json(constant: str) -> None:
        raise AssertionError(f"the token file holds {constant}, which is not JSON")

    assert json.loads(login.token_path.read_text(), parse_constant=not_json)["expires_at"] == clock.now + expires_in


def test_an_answer_without_a_refresh_token_still_signs_in(tmp_path: Path, caplog: pytest.LogCaptureFixture) -> None:
    caplog.set_level(logging.INFO)
    login, _, _ = composio(tmp_path, **{TOKEN: granted(refresh_token=None, scope=None)})
    run(login.finish("code-1", state_of(run(login.start(CP_CALLBACK)))))
    data = json.loads(login.token_path.read_text())
    assert data["refresh_token"] == "" and data["access_token"] == ACCESS
    assert data["scope"] == onboarding.CONNECT_SCOPE                       # the server may leave an unchanged scope out
    assert "no refresh token" in caplog.text


@pytest.mark.parametrize("case, line", [(0, onboarding.COMPOSIO_GRANT_EXPIRED), (2, onboarding.COMPOSIO_CLIENT_GONE)])
def test_the_token_endpoints_two_refusals_are_plain_lines(tmp_path: Path, case: int, line: str) -> None:
    probe = TOKEN_PROBE["cases"][case]                                       # invalid_grant, then invalid_client
    login, fake, _ = composio(tmp_path, **{TOKEN: reply(probe["status"], probe["answer"])})
    with pytest.raises(OnboardingError) as e:
        run(login.finish("code-1", state_of(run(login.start(CP_CALLBACK)))))
    assert str(e.value) == line and not login.token_path.exists()
    run(login.start(CP_CALLBACK))
    assert len(fake.to(REGISTER)) == (2 if probe["answer"]["error"] == "invalid_client" else 1)   # gone: again


def test_a_client_gone_from_the_token_file_is_registered_again(tmp_path: Path) -> None:
    path = tmp_path / "composio-connect.json"
    path.write_text(json.dumps({"version": 1, "client_id": "client-kept-in-file", "redirect_uris": CP_FORMS}))
    gone = TOKEN_PROBE["cases"][2]
    login, fake, _ = composio(tmp_path, **{TOKEN: reply(gone["status"], gone["answer"])})
    with pytest.raises(OnboardingError):
        run(login.finish("code-1", state_of(run(login.start(CP_CALLBACK)))))
    assert query(run(login.start(CP_CALLBACK)))["client_id"] == [CLIENT_ID]
    assert len(fake.to(REGISTER)) == 1


@pytest.mark.parametrize("code, error, line", [
    ("", "access_denied", onboarding.COMPOSIO_CANCELLED),
    ("", "invalid_request", onboarding.COMPOSIO_NOT_FINISHED),                # S1: PKCE "plain" comes back so
    ("code-1", "server_error", onboarding.COMPOSIO_NOT_FINISHED),
    ("", "", onboarding.COMPOSIO_NOT_FINISHED),
    ("x" * (onboarding.MAX_CODE_CHARS + 1), "", onboarding.COMPOSIO_NOT_FINISHED),
])
def test_a_refused_or_codeless_callback_sends_nothing(tmp_path: Path, code: str, error: str, line: str) -> None:
    login, fake, _ = composio(tmp_path)
    with pytest.raises(OnboardingError) as e:
        run(login.finish(code, state_of(run(login.start(CP_CALLBACK))), error=error))
    assert str(e.value) == line and e.value.status == 400
    assert fake.to(TOKEN) == [] and not login.token_path.exists()


@pytest.mark.parametrize("route, line, status", [
    (reply(500, {"error": "server_error"}), onboarding.COMPOSIO_FAILED.format(status=500), 502),
    (reply(200, {"token_type": "Bearer"}), onboarding.COMPOSIO_NO_TOKEN, 502),
    (reply(200, {"access_token": "two words"}), onboarding.COMPOSIO_NO_TOKEN, 502),
    (httpx.ConnectError, onboarding.COMPOSIO_UNREACHABLE, 502),
])
def test_every_other_composio_failure_is_a_plain_line(tmp_path: Path, route: Any, line: str, status: int) -> None:
    login, _, _ = composio(tmp_path, **{TOKEN: route})
    with pytest.raises(OnboardingError) as e:
        run(login.finish("code-1", state_of(run(login.start(CP_CALLBACK)))))
    assert str(e.value) == line and e.value.status == status and not login.token_path.exists()


@pytest.mark.skipif(ROOT, reason="root ignores folder permissions")
def test_a_token_file_that_cannot_be_written_is_a_plain_line(tmp_path: Path) -> None:
    folder = tmp_path / "locked"
    folder.mkdir()
    login, _, _ = composio(folder)
    url = run(login.start(CP_CALLBACK))
    os.chmod(folder, 0o500)
    try:
        with pytest.raises(OnboardingError) as e:
            run(login.finish("code-1", state_of(url)))
    finally:
        os.chmod(folder, 0o700)
    assert e.value.status == 500 and ACCESS not in str(e.value)
    assert list(folder.iterdir()) == []


def test_the_tokens_are_hidden_from_every_log_record_once_they_arrive(tmp_path: Path) -> None:
    login, _, _ = composio(tmp_path)
    run(login.finish("code-1", state_of(run(login.start(CP_CALLBACK)))))
    record = logging.getLogRecordFactory()("httpx", logging.INFO, __file__, 1, f"{ACCESS} {REFRESH}", None, None)
    assert ACCESS not in record.getMessage() and REFRESH not in record.getMessage()


# ---- canaries: a key or token reaches nothing but its own file --------------------------------------------

def scan(canary: str, caplog: pytest.LogCaptureFixture, roots: list[Path]) -> dict[str, Any]:
    """Where ``canary`` shows: the log (each record rendered, with any traceback, then the handler's own text) and
    the files under ``roots``."""
    rendered = (r.getMessage() + (r.exc_text or "") for r in caplog.records)
    logs = [line for line in rendered if canary in line]
    if not logs and canary in caplog.text:
        logs = ["caplog.text"]
    files = sorted(p for root in roots for p in root.rglob("*") if p.is_file() and canary.encode() in p.read_bytes())
    return {"logs": logs, "files": files}


def test_a_canary_key_shows_nowhere_but_the_env_file(tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
                                                     caplog: pytest.LogCaptureFixture,
                                                     capsys: pytest.CaptureFixture[str], _test_dir: Path) -> None:
    hidden: list[str] = []
    monkeypatch.setattr(onboarding, "_hide", lambda *values: hidden.extend(values))   # no redaction to mask a leak
    monkeypatch.setenv("HOME", str(tmp_path / "home"))
    caplog.set_level(logging.DEBUG)
    roots = [tmp_path, _test_dir]
    logging.getLogger("eve_bridge.onboarding").debug("control %s", KEY)          # positive control: logs
    (tmp_path / "control").write_text(KEY)                                          # positive control: files
    assert scan(KEY, caplog, roots) == {"logs": [f"control {KEY}"], "files": [tmp_path / "control"]}
    caplog.clear()
    (tmp_path / "control").unlink()

    login, fake, _ = openrouter(**{KEY_INFO: reply(200, key_info()), CREDITS: reply(403, MANAGEMENT_ONLY)})
    url = login.start(OR_CALLBACK, "")
    key = run(login.finish("code-1", state_of(url)))
    env: dict[str, str] = {}
    save_openrouter_key(key, environ=env)
    shown = [run(credit(key, http=fake.http))]
    fake.routes[KEY_INFO] = reply(401, {"error": {"code": 401, "message": f"bad key {KEY}"}})
    shown.append(run(credit(key, http=fake.http)))
    fake.routes[EXCHANGE] = reply(403, {"error": {"code": 403, "message": f"Invalid code for {KEY}"}})
    with pytest.raises(OnboardingError) as e:
        run(login.finish("code-2", state_of(login.start(OR_CALLBACK, ""))))

    assert hidden[:2] == [KEY, KEY] and set(hidden) == {KEY}     # handed to the redaction on arrival and on save
    assert scan(KEY, caplog, roots) == {"logs": [], "files": [tmp_path / "home" / ".config" / "eve-bridge" / "env"]}
    out, err = capsys.readouterr()
    assert KEY not in out + err
    assert KEY not in json.dumps(shown) and KEY not in str(e.value) and KEY not in url
    for request in fake.seen:                                    # sent only as the bearer of the two GETs
        assert KEY.encode() not in request.content and KEY not in str(request.url)
        if KEY in request.headers.get("authorization", ""):
            assert request.method == "GET" and str(request.url) in (spend_sync.OPENROUTER_KEY_URL,
                                                                    spend_sync.OPENROUTER_CREDITS_URL)
    assert any(KEY in r.headers.get("authorization", "") for r in fake.seen)


def test_canary_tokens_show_nowhere_but_the_token_file(tmp_path: Path, monkeypatch: pytest.MonkeyPatch,
                                                       caplog: pytest.LogCaptureFixture,
                                                       capsys: pytest.CaptureFixture[str], _test_dir: Path) -> None:
    hidden: list[str] = []
    monkeypatch.setattr(onboarding, "_hide", lambda *values: hidden.extend(values))
    caplog.set_level(logging.DEBUG)
    roots = [tmp_path, _test_dir]
    logging.getLogger("eve_bridge.onboarding").debug("control %s %s", ACCESS, CLIENT_ID)
    assert scan(ACCESS, caplog, roots)["logs"] and scan(CLIENT_ID, caplog, roots)["logs"]     # positive control
    caplog.clear()

    login, fake, _ = composio(tmp_path)
    run(login.finish("code-1", state_of(run(login.start(CP_CALLBACK)))))
    fake.routes[TOKEN] = reply(400, {"error": "invalid_grant", "error_description": f"bad {REFRESH}"})
    with pytest.raises(OnboardingError) as e:
        run(login.finish("code-2", state_of(run(login.start(CP_CALLBACK)))))

    assert hidden == [ACCESS, REFRESH]
    token_file = [tmp_path / "composio-connect.json"]
    for canary in (ACCESS, REFRESH, ID_TOKEN):
        assert scan(canary, caplog, roots) == {"logs": [], "files": token_file if canary != ID_TOKEN else []}
    assert scan(CLIENT_ID, caplog, roots)["logs"] == []           # the client id is not logged either
    out, err = capsys.readouterr()
    assert not any(c in out + err for c in (ACCESS, REFRESH, ID_TOKEN))
    assert REFRESH not in str(e.value)
    for request in fake.seen:                                     # eve never sends a token anywhere
        assert not any(c.encode() in request.content for c in (ACCESS, REFRESH, ID_TOKEN))
        assert not any(c in str(request.headers) for c in (ACCESS, REFRESH, ID_TOKEN))


# ---- the label ---------------------------------------------------------------------------------------------

def test_mac_label_comes_from_the_host_name_without_starting_a_program(monkeypatch: pytest.MonkeyPatch) -> None:
    def refuse(*_a: Any, **_k: Any) -> None:
        raise AssertionError("a program was started")

    monkeypatch.setattr(subprocess, "Popen", refuse)
    monkeypatch.setattr(onboarding.socket, "gethostname", lambda: "Test-Mac.local")
    assert onboarding.mac_label() == "eve lite on Test-Mac"
    monkeypatch.setattr(onboarding.socket, "gethostname", lambda: "")
    assert onboarding.mac_label() == "eve lite on this Mac"

    def broken() -> str:
        raise OSError("no name")

    monkeypatch.setattr(onboarding.socket, "gethostname", broken)
    assert onboarding.mac_label() == "eve lite on this Mac"


def test_a_label_is_one_short_printable_line(monkeypatch: pytest.MonkeyPatch, tmp_path: Path) -> None:
    monkeypatch.setattr(onboarding.socket, "gethostname", lambda: "Test-Mac.local")
    label = query(OpenRouterLogin().start(OR_CALLBACK, "eve lite\non\x00 a Mac " + "x" * 200))["key_label"][0]
    assert label.startswith("eve lite on a Mac x") and len(label) <= onboarding.LABEL_MAX
    assert label.isprintable()
    assert query(OpenRouterLogin().start(OR_CALLBACK, ""))["key_label"] == ["eve lite on Test-Mac"]
    assert query(OpenRouterLogin().start(OR_CALLBACK))["key_label"] == ["eve lite on Test-Mac"]
    login = ComposioConnectLogin(tmp_path / "composio-connect.json", http=Provider({REGISTER: registered}).http)
    run(login.start(CP_CALLBACK))
    assert login._client_name == "eve lite on Test-Mac"
