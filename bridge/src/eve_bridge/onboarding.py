"""eve lite's two browser sign-ins: OpenRouter mints the model key, Composio Connect grants the apps.

Owner, 2026-10-13: "use composio and openrouter as the only setup things". Neither sign-in asks anyone to paste a
key. Each is an OAuth authorization-code flow with PKCE (S256) that comes back to eve's own page on this Mac,
``http://localhost:<port>`` (docs/plans/eve-lite.md, M3). The setup page calls ``start``, sends the browser to
the URL it returns, and hands the callback's query to ``finish``.

* **The callback is checked by ``state`` alone.** The setup page's ``eve_mac`` cookie is ``SameSite=Strict``, and
  a browser does not send it on the provider's cross-site redirect back. So each start mints a 128-bit ``state``
  that is good for one callback within ``STATE_TTL_SECS``. A missing, unknown, used or expired one is refused
  (``StateError``, HTTP 400) before anything leaves the Mac. At most ``MAX_PENDING`` sign-ins wait at once; one
  more drops the oldest.
* **OpenRouter** (``OpenRouterLogin``). The browser goes to ``https://openrouter.ai/auth`` with ``callback_url``,
  the challenge, ``state`` and ``key_label``. OpenRouter titles a localhost app ``localhost:<port>`` and returns
  ``state`` unchanged, but not on a denial. The code is exchanged at ``/api/v1/auth/keys`` with the JSON body
  ``{code, code_verifier, code_challenge_method}``, and the answer's ``key`` is the model key.
  ``save_openrouter_key`` writes it to the env file (0600, atomically) and to this process's environment;
  ``credit`` reads what it can spend.
* **Composio Connect** (``ComposioConnectLogin``), as spike S1 measured it on 2026-10-13
  (docs/plans/eve-lite-s1.md). eve registers itself once as a public client (RFC 7591, no secret) with both
  loopback forms of the bound port's callback. The authorize request carries ``resource``, so the token is good
  at ``/mcp``. The token endpoint is on ``login.composio.dev``, not the issuer's host, so every endpoint is pinned
  here rather than derived from the issuer. The exchange is a form POST with no secret and no Basic header.
  ``composio-connect.json`` (0600) is written only once a token arrives, so the file existing means "signed in".
  It keeps the client id, which is reused while it lists the callback (Composio cannot delete a client).

Failures raise ``OnboardingError``. ``str(e)`` is one plain line for the setup page, never a key, a code or a
provider's text. ``e.status`` is the HTTP status for the callback's answer. A key or token is handed to
``telegram.hide_token`` the moment it arrives, and no log line here carries a key, token, code, state, verifier or
client id (AGENTS rule 10).
"""

from __future__ import annotations

import asyncio
import base64
import contextlib
import hmac
import json
import logging
import math
import os
import re
import secrets
import socket
import time
import urllib.parse
from dataclasses import dataclass, field
from pathlib import Path
from typing import Any, AsyncIterator, Callable, Mapping, MutableMapping, Optional

import httpx

from . import envfile, openrouter, spend_sync
from .apps_mcp import _pkce, _read_json, write_private_json

log = logging.getLogger(__name__)

STATE_TTL_SECS = 600.0          # a sign-in left open longer starts again; OpenRouter's codes live 10 minutes too
MAX_PENDING = 8
HTTP_TIMEOUT_SECS = 20.0
CREDIT_TIMEOUT_SECS = 15.0
LABEL_MAX = 60
MAX_CODE_CHARS = 4096

OPENROUTER_AUTH_URL = "https://openrouter.ai/auth"
OPENROUTER_KEYS_URL = "https://openrouter.ai/api/v1/auth/keys"

# Composio Connect's authorization-server document, as S1 read it (bridge/tests/fixtures/composio_connect).
CONNECT_ISSUER = "https://connect.composio.dev"
CONNECT_AUTHORIZE_URL = "https://connect.composio.dev/oauth/authorize"
CONNECT_TOKEN_URL = "https://login.composio.dev/oauth2/token"
CONNECT_REGISTER_URL = "https://login.composio.dev/oauth2/register"
CONNECT_RESOURCE = "https://connect.composio.dev/mcp"
CONNECT_SCOPE = "openid profile email offline_access"
CONNECT_TOKEN_FILE = Path("~/.config/eve-bridge/composio-connect.json")
TOKEN_FILE_VERSION = 1
DEFAULT_TOKEN_SECS = 300.0      # neither expires_in nor the token says: refresh early rather than late

# What the setup page shows: one plain line each.
STATE_MISSING = "That sign-in was cancelled or did not finish. Start it again from eve's setup page."
STATE_UNKNOWN = "That sign-in link was already used or is out of date. Start it again from eve's setup page."
STATE_EXPIRED = "That sign-in took longer than 10 minutes. Start it again from eve's setup page."
OPENROUTER_NOT_AUTHORIZED = "OpenRouter did not authorize eve. Sign in with OpenRouter again and click Authorize."
OPENROUTER_CODE_EXPIRED = "OpenRouter's sign-in code expired: it lasts 10 minutes. Sign in with OpenRouter again."
OPENROUTER_REFUSED = "OpenRouter did not accept that sign-in. Sign in with OpenRouter again."
OPENROUTER_FAILED = "OpenRouter could not finish the sign-in (HTTP {status}). Try again in a minute."
OPENROUTER_UNREACHABLE = "eve could not reach OpenRouter. Check this Mac's internet connection and try again."
OPENROUTER_NO_KEY = "OpenRouter answered without a key. Sign in with OpenRouter again."
COMPOSIO_CANCELLED = "Composio sign-in was cancelled. You can sign in later from Settings → Apps."
COMPOSIO_NOT_FINISHED = "Composio did not finish the sign-in. Sign in with Composio again."
COMPOSIO_GRANT_EXPIRED = "That Composio sign-in expired or was already used. Sign in with Composio again."
COMPOSIO_CLIENT_GONE = "Composio no longer knows eve on this Mac. Sign in with Composio again."
COMPOSIO_FAILED = "Composio could not finish the sign-in (HTTP {status}). Try again in a minute."
COMPOSIO_UNREACHABLE = "eve could not reach Composio. Check this Mac's internet connection and try again."
COMPOSIO_NO_TOKEN = "Composio answered without a sign-in token. Sign in with Composio again."
COMPOSIO_NOT_REGISTERED = "Composio did not register eve on this Mac (HTTP {status}). Try again in a minute."
COMPOSIO_NO_CALLBACK = "Composio did not accept eve's sign-in address on this Mac. Try again in a minute."
NOT_SAVED = "eve could not save the sign-in on this Mac ({kind}). Check the disk, then sign in again."

_LOOPBACK = ("localhost", "127.0.0.1", "::1")
_KEY_SHAPE = re.compile(r"[\x21-\x7e]{8,512}")          # one visible-ASCII word: no space, no line break
_TOKEN_SHAPE = re.compile(r"[\x21-\x7e]{8,16384}")      # an access token may be a long JWT
_ID_SHAPE = re.compile(r"[\x21-\x7e]{1,256}")
_OAUTH_WORD = re.compile(r"[a-z_]{1,40}")               # an OAuth error code, safe to log
_MAX_TOKEN_SECS = 10 * 365 * 86400.0                    # beyond this an expiry is nonsense, not a lifetime
_warned: set[str] = set()


class OnboardingError(Exception):
    """A sign-in that did not finish. ``str(e)`` is one plain line for the setup page; ``status`` is the HTTP
    status the callback answers with (400: the browser's side, 502: the provider's, 500: this Mac's)."""

    def __init__(self, line: str, status: int = 502) -> None:
        super().__init__(line)
        self.status = status


class StateError(OnboardingError):
    """The callback's ``state`` is missing, unknown, used or expired. Raised before any request leaves the Mac."""

    def __init__(self, line: str) -> None:
        super().__init__(line, 400)


def mac_label() -> str:
    """"eve lite on <this Mac>", the label OpenRouter gives the key and the name Composio gives the client, so the
    person can tell them apart in their accounts. Read from the host name: the lite firewall lets almost no
    program start, so ``scutil --get ComputerName`` is not an option."""
    try:
        name = socket.gethostname()
    except OSError:
        name = ""
    name = "".join(c for c in name.strip().removesuffix(".local") if c.isprintable())[:40].strip()
    return f"eve lite on {name or 'this Mac'}"


@dataclass(frozen=True)
class _Pending:
    """What one waiting sign-in's callback needs."""
    verifier: str = field(repr=False)
    redirect_uri: str
    client_id: str = ""
    redirect_uris: tuple[str, ...] = ()


class _States:
    """The sign-ins waiting for their callback, by ``state``. Each is good once, for ``STATE_TTL_SECS``."""

    def __init__(self, clock: Callable[[], float]) -> None:
        self._clock = clock
        self._waiting: dict[str, tuple[float, _Pending]] = {}

    def __len__(self) -> int:
        return len(self._waiting)

    def issue(self, pending: _Pending) -> str:
        now = self._clock()
        self._waiting = {s: w for s, w in self._waiting.items() if w[0] > now}
        while len(self._waiting) >= MAX_PENDING:
            self._waiting.pop(next(iter(self._waiting)))          # the oldest
        state = secrets.token_urlsafe(16)                          # 128 bits
        self._waiting[state] = (now + STATE_TTL_SECS, pending)
        return state

    def take(self, state: Any) -> _Pending:
        """The sign-in ``state`` belongs to, which is then used up. StateError when there is none."""
        if not isinstance(state, str) or not state:
            raise StateError(STATE_MISSING)
        given = state.encode("utf-8", "replace")
        match = next((s for s in self._waiting if hmac.compare_digest(s.encode(), given)), None)
        if match is None:
            raise StateError(STATE_UNKNOWN)
        expires, pending = self._waiting.pop(match)
        if expires <= self._clock():
            raise StateError(STATE_EXPIRED)
        return pending


def _loopback(url: str) -> urllib.parse.SplitResult:
    """``url`` checked as eve's own callback on this Mac: plain http on a loopback host and port, no credentials,
    query or fragment (both providers append their own query). ValueError otherwise: the onboarding routes live
    only on the local listener, so anything else is a wiring mistake."""
    parts = urllib.parse.urlsplit(url if isinstance(url, str) else "")
    try:
        port = parts.port
    except ValueError:
        port = None
    if (parts.scheme != "http" or parts.hostname not in _LOOPBACK or not port or parts.username is not None
            or parts.password is not None or parts.query or parts.fragment or not parts.path.startswith("/")):
        raise ValueError("onboarding: the callback must be eve's own http://localhost:<port>/... page")
    return parts


def _redirect_forms(parts: urllib.parse.SplitResult) -> list[str]:
    """The two loopback forms of the callback that S1 registered and Composio accepted: localhost, then 127.0.0.1."""
    return [f"http://{host}:{parts.port}{parts.path}" for host in ("localhost", "127.0.0.1")]


def _redirect_uri(parts: urllib.parse.SplitResult) -> str:
    """The callback Composio is told: the page's own form, except that an IPv6 literal comes back on localhost,
    a form S1 tested. (OpenRouter is always told the localhost form: localhost is what it documents.)"""
    forms = _redirect_forms(parts)
    return forms[1] if parts.hostname == "127.0.0.1" else forms[0]


def _clean_label(label: str) -> str:
    text = " ".join(str(label or "").split())
    return "".join(c for c in text if c.isprintable())[:LABEL_MAX].strip() or mac_label()


@contextlib.asynccontextmanager
async def _client(http: Optional[httpx.AsyncClient], timeout: float = HTTP_TIMEOUT_SECS
                  ) -> AsyncIterator[httpx.AsyncClient]:
    """The client to call with: the one handed in (tests pass one on a mock transport), else a fresh one."""
    if http is not None:
        yield http
        return
    async with httpx.AsyncClient(timeout=timeout) as client:
        yield client


def _answer(response: httpx.Response) -> dict[str, Any]:
    try:
        data = response.json()
    except ValueError:
        return {}
    return data if isinstance(data, dict) else {}


def _hide(*values: str) -> None:
    """Keep these out of every log record in the process from now on, whoever writes it. ``hide_token`` wraps the
    record factory; it is idempotent and ignores ""."""
    from .telegram import hide_token

    for value in values:
        if value:
            hide_token(value)


def _code_shaped(code: Any) -> bool:
    """An authorization code worth sending on: a non-empty string of sane length. Codes are opaque and short; a
    huge or empty one is refused here rather than posted to the provider."""
    return isinstance(code, str) and 0 < len(code) <= MAX_CODE_CHARS


def _oauth_word(answer: Mapping[str, Any]) -> str:
    """The answer's OAuth ``error`` code when it looks like one (safe to log and to branch on), else ""."""
    word = answer.get("error")
    return word if isinstance(word, str) and _OAUTH_WORD.fullmatch(word) else ""


# ---- OpenRouter -------------------------------------------------------------------------------------------

class OpenRouterLogin:
    """OpenRouter's PKCE sign-in, which mints eve's one model key: ``start``, the browser, then ``finish``."""

    def __init__(self, *, http: Optional[httpx.AsyncClient] = None,
                 clock: Callable[[], float] = time.time) -> None:
        self._http = http
        self._states = _States(clock)

    def start(self, callback_url: str, label: str = "") -> str:
        """The URL to send the browser to. ``callback_url`` is eve's own loopback callback, which OpenRouter is
        given in its ``localhost`` form, the one it documents for any port; ``label`` names the key in the person's
        OpenRouter account (default: ``mac_label()``)."""
        redirect = _redirect_forms(_loopback(callback_url))[0]
        verifier, challenge = _pkce()
        state = self._states.issue(_Pending(verifier, redirect))
        return OPENROUTER_AUTH_URL + "?" + urllib.parse.urlencode({
            "callback_url": redirect, "code_challenge": challenge, "code_challenge_method": "S256",
            "state": state, "key_label": _clean_label(label)}, quote_via=urllib.parse.quote)

    async def finish(self, code: str, state: str, *, error: str = "") -> str:
        """The key, for the callback's ``code`` and ``state``. ``error`` is the callback's own, when it has one.
        StateError (400) with no request made for a bad ``state``; OnboardingError for anything else."""
        pending = self._states.take(state)
        if error or not _code_shaped(code):
            raise OnboardingError(OPENROUTER_NOT_AUTHORIZED, 400)
        body = {"code": code, "code_verifier": pending.verifier, "code_challenge_method": "S256"}
        try:
            async with _client(self._http) as client:
                response = await client.post(OPENROUTER_KEYS_URL, json=body)
        except httpx.HTTPError as e:
            log.warning("onboarding: the OpenRouter key exchange failed (%s)", type(e).__name__)
            raise OnboardingError(OPENROUTER_UNREACHABLE) from None
        answer = _answer(response)
        if response.status_code != 200:
            log.warning("onboarding: OpenRouter refused the key exchange (HTTP %d)", response.status_code)
            if response.status_code == 403 and "expire" in _openrouter_reason(answer).lower():
                raise OnboardingError(OPENROUTER_CODE_EXPIRED)
            if response.status_code in (400, 403):
                raise OnboardingError(OPENROUTER_REFUSED)
            raise OnboardingError(OPENROUTER_FAILED.format(status=response.status_code))
        key = answer.get("key")
        key = key.strip() if isinstance(key, str) else ""
        if not _KEY_SHAPE.fullmatch(key):
            log.warning("onboarding: OpenRouter's answer held no usable key")
            raise OnboardingError(OPENROUTER_NO_KEY)
        _hide(key)
        log.info("onboarding: OpenRouter minted a key")
        return key


def _openrouter_reason(answer: Mapping[str, Any]) -> str:
    """OpenRouter's error message ("Authorization code expired"), to tell its two 403s apart. Never shown or
    logged."""
    error = answer.get("error")
    message = error.get("message") if isinstance(error, Mapping) else error
    return message if isinstance(message, str) else str(answer.get("message") or "")


def save_openrouter_key(key: str, *, path: Optional[Path] = None,
                        environ: MutableMapping[str, str] = os.environ) -> None:
    """Keep the key OpenRouter minted. It is hidden from every log line, written to the env file the daemon reads
    (``envfile.set_key``: 0600, atomic, the file's other lines kept) and set in ``environ``. The caller restarts the
    daemon. ValueError for a value that is not a key; OnboardingError (500) when the file cannot be written."""
    if not isinstance(key, str) or not _KEY_SHAPE.fullmatch(key):
        raise ValueError("onboarding: that is not an OpenRouter key")
    _hide(key)
    target = path if path is not None else envfile.env_path(environ)
    try:
        envfile.set_key(target, openrouter.KEY_ENV, key)
    except (OSError, ValueError) as e:
        log.warning("onboarding: the OpenRouter key was not saved (%s)", type(e).__name__)
        raise OnboardingError(NOT_SAVED.format(kind=type(e).__name__), 500) from None
    environ[openrouter.KEY_ENV] = key
    log.info("onboarding: saved %s", openrouter.KEY_ENV)


REJECTED_WHY = "HTTP 401"            # /api/v1/key answering 401: the key was deleted or revoked, not a passing error


async def credit(key: str, *, http: Optional[httpx.AsyncClient] = None) -> dict[str, Any]:
    """What the key can spend, for the setup page: ``{"ok", "usd", "funded"}``, plus ``"error"`` (a short reason:
    "HTTP 401", "timed out") when the key itself could not be read, and ``"rejected": True`` when that reason is a
    401 (the key no longer works and a new sign-in is the only fix, not a passing network error). Never raises, and
    never holds the key.

    ``usd`` is the smaller of the account's balance (``/api/v1/credits``: bought less used) and the key's own
    remaining limit (``limit_remaining``, null for a key with no limit, which is how OAuth mints them). It is None
    when neither can be read: OpenRouter documents ``/credits`` as needing a management key, which an OAuth key is
    not. ``funded`` is whether a paid call can go through: usd above zero; without a figure, False only when the
    account has never bought credits (``is_free_tier``), else None (not known)."""
    if not isinstance(key, str) or not key.strip():
        return {"ok": False, "usd": None, "funded": None, "error": "no key"}
    _hide(key.strip())                   # after a restart the key comes from the env file, not from finish
    auth = {"Authorization": f"Bearer {key.strip()}", "Accept": "application/json"}
    try:
        async with _client(http, CREDIT_TIMEOUT_SECS) as client:
            (info, why), (account, _) = await asyncio.gather(_read(client, spend_sync.OPENROUTER_KEY_URL, auth),
                                                             _read(client, spend_sync.OPENROUTER_CREDITS_URL, auth))
    except Exception as e:  # noqa: BLE001 — the page asks again; the type is the reason, never the key
        info, why, account = None, type(e).__name__, None
    if info is None:
        if why not in _warned:
            _warned.add(why)
            log.warning("onboarding: the OpenRouter key could not be read (%s)", why)
        answer = {"ok": False, "usd": None, "funded": None, "error": why}
        if why == REJECTED_WHY:
            answer["rejected"] = True        # the key endpoint turned the key down: it must be signed in again
        return answer
    left: list[float] = []
    if account is not None:
        bought, used = _num(account.get("total_credits")), _num(account.get("total_usage"))
        if bought is not None and used is not None:
            left.append(bought - used)
    limit_left = _num(info.get("limit_remaining"))
    if limit_left is not None:
        left.append(limit_left)
    if left:
        usd = max(0.0, round(min(left), 2))
        return {"ok": True, "usd": usd, "funded": usd > 0}
    return {"ok": True, "usd": None, "funded": False if info.get("is_free_tier") is True else None}


async def _read(client: httpx.AsyncClient, url: str, headers: Mapping[str, str]
                ) -> tuple[Optional[dict[str, Any]], str]:
    """``data`` of a GET, or None and a short reason. Never raises."""
    try:
        response = await client.get(url, headers=dict(headers))
    except httpx.TimeoutException:
        return None, "timed out"
    except Exception:  # noqa: BLE001 — any transport failure reads the same on the page
        return None, "unreachable"
    if response.status_code != 200:
        return None, f"HTTP {response.status_code}"
    data = _answer(response).get("data")
    return (data, "") if isinstance(data, dict) else (None, "unreadable reply")


def _num(value: Any) -> Optional[float]:
    """A finite number, else None. A provider's figure is a plain JSON number; anything else (a string, a bool, an
    integer too big for a float, Infinity or NaN) reads as unknown, so it never reaches the page's JSON."""
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    try:
        number = float(value)
    except OverflowError:
        return None
    return number if math.isfinite(number) else None


# ---- Composio Connect -------------------------------------------------------------------------------------

@dataclass(frozen=True)
class _Client:
    """eve's registration at Composio: a public client, so the id is all there is."""
    client_id: str
    redirect_uris: tuple[str, ...]


class ComposioConnectLogin:
    """Composio Connect's OAuth sign-in for eve lite's apps: ``start``, the browser, then ``finish``, which writes
    ``token_path`` (``composio-connect.json``) for the ConnectBridge."""

    def __init__(self, token_path: Path = CONNECT_TOKEN_FILE, *, client_name: str = "",
                 http: Optional[httpx.AsyncClient] = None, clock: Callable[[], float] = time.time) -> None:
        self.token_path = Path(token_path).expanduser()
        self._client_name = _clean_label(client_name)
        self._http = http
        self._clock = clock
        self._states = _States(clock)
        self._registered: Optional[_Client] = None
        self._gone: set[str] = set()              # client ids the token endpoint no longer knows
        self._lock = asyncio.Lock()

    async def start(self, callback_url: str) -> str:
        """The URL to send the browser to. Registers eve first when no known registration lists this callback.
        OnboardingError when Composio cannot be reached or refuses the registration."""
        parts = _loopback(callback_url)
        redirect = _redirect_uri(parts)
        client = await self._client_for(_redirect_forms(parts), redirect)
        verifier, challenge = _pkce()
        state = self._states.issue(_Pending(verifier, redirect, client.client_id, client.redirect_uris))
        return CONNECT_AUTHORIZE_URL + "?" + urllib.parse.urlencode({
            "response_type": "code", "client_id": client.client_id, "redirect_uri": redirect,
            "scope": CONNECT_SCOPE, "state": state, "code_challenge": challenge,
            "code_challenge_method": "S256", "resource": CONNECT_RESOURCE}, quote_via=urllib.parse.quote)

    async def finish(self, code: str, state: str, *, error: str = "") -> None:
        """Exchange the callback's ``code`` and write the token file (0600). ``error`` is the callback's own, when
        it has one: Composio sends ``error`` and ``state``, and no ``iss``, when it refuses. StateError (400) with no
        request made for a bad ``state``; OnboardingError for anything else, and then no file is written."""
        pending = self._states.take(state)
        if error:
            log.info("onboarding: Composio sent the sign-in back with an error (%s)",
                     error if _OAUTH_WORD.fullmatch(str(error)) else "?")
            raise OnboardingError(COMPOSIO_CANCELLED if error == "access_denied" else COMPOSIO_NOT_FINISHED, 400)
        if not _code_shaped(code):
            raise OnboardingError(COMPOSIO_NOT_FINISHED, 400)
        form = {"grant_type": "authorization_code", "code": code, "redirect_uri": pending.redirect_uri,
                "client_id": pending.client_id, "code_verifier": pending.verifier, "resource": CONNECT_RESOURCE}
        try:
            async with _client(self._http) as client:
                response = await client.post(CONNECT_TOKEN_URL, data=form, headers={"Accept": "application/json"})
        except httpx.HTTPError as e:
            log.warning("onboarding: the Composio token exchange failed (%s)", type(e).__name__)
            raise OnboardingError(COMPOSIO_UNREACHABLE) from None
        answer = _answer(response)
        if response.status_code != 200:
            word = _oauth_word(answer)
            log.warning("onboarding: Composio refused the token exchange (HTTP %d, %s)", response.status_code,
                        word or "no error code")
            if word == "invalid_client" or response.status_code == 401:
                self._forget(pending.client_id)                   # S1: the client is gone, so register again
                raise OnboardingError(COMPOSIO_CLIENT_GONE)
            if word == "invalid_grant":
                raise OnboardingError(COMPOSIO_GRANT_EXPIRED)
            raise OnboardingError(COMPOSIO_FAILED.format(status=response.status_code))
        access, refresh = answer.get("access_token"), answer.get("refresh_token")
        if not isinstance(access, str) or not _TOKEN_SHAPE.fullmatch(access):
            log.warning("onboarding: Composio's answer held no usable access token")
            raise OnboardingError(COMPOSIO_NO_TOKEN)
        refresh = refresh if isinstance(refresh, str) and _TOKEN_SHAPE.fullmatch(refresh) else ""
        _hide(access, refresh)
        scope = answer.get("scope")
        try:
            write_private_json(self.token_path, {
                "version": TOKEN_FILE_VERSION, "issuer": CONNECT_ISSUER, "client_id": pending.client_id,
                "redirect_uris": list(pending.redirect_uris), "access_token": access, "refresh_token": refresh,
                "expires_at": _expires_at(answer, access, self._clock()),
                "scope": scope if isinstance(scope, str) and scope.strip() else CONNECT_SCOPE})
        except OSError as e:
            log.warning("onboarding: the Composio sign-in was not saved (%s)", type(e).__name__)
            raise OnboardingError(NOT_SAVED.format(kind=type(e).__name__), 500) from None
        self._registered = _Client(pending.client_id, pending.redirect_uris)
        if not refresh:
            log.warning("onboarding: Composio gave no refresh token; the sign-in lapses with its access token")
        log.info("onboarding: signed in to Composio Connect")

    async def _client_for(self, forms: list[str], redirect: str) -> _Client:
        """A registration that lists ``redirect``: the one made in this process, else the token file's, else a
        new one. One at a time, so two quick clicks register once."""
        async with self._lock:
            for known in (self._registered, self._stored()):
                if known is not None and known.client_id not in self._gone and redirect in known.redirect_uris:
                    self._registered = known
                    return known
            self._registered = await self._register(forms, redirect)
            return self._registered

    def _stored(self) -> Optional[_Client]:
        data = _read_json(self.token_path)
        client_id, uris = data.get("client_id"), data.get("redirect_uris")
        if (data.get("version") != TOKEN_FILE_VERSION or not isinstance(client_id, str)
                or not _ID_SHAPE.fullmatch(client_id) or not isinstance(uris, list)):
            return None
        return _Client(client_id, tuple(u for u in uris if isinstance(u, str)))

    async def _register(self, forms: list[str], redirect: str) -> _Client:
        """Dynamic client registration with the body S1 verified: a public client with both loopback callbacks."""
        body = {"client_name": self._client_name, "redirect_uris": forms, "token_endpoint_auth_method": "none",
                "grant_types": ["authorization_code", "refresh_token"], "response_types": ["code"],
                "scope": CONNECT_SCOPE}
        try:
            async with _client(self._http) as client:
                response = await client.post(CONNECT_REGISTER_URL, json=body,
                                             headers={"Accept": "application/json"})
        except httpx.HTTPError as e:
            log.warning("onboarding: the Composio registration failed (%s)", type(e).__name__)
            raise OnboardingError(COMPOSIO_UNREACHABLE) from None
        answer = _answer(response)
        client_id = answer.get("client_id")
        if (response.status_code not in (200, 201) or not isinstance(client_id, str)
                or not _ID_SHAPE.fullmatch(client_id)):
            log.warning("onboarding: Composio refused the registration (HTTP %d, %s)", response.status_code,
                        _oauth_word(answer) or "no error code")
            raise OnboardingError(COMPOSIO_NOT_REGISTERED.format(status=response.status_code))
        uris = answer.get("redirect_uris")
        registered = tuple(u for u in uris if isinstance(u, str)) if isinstance(uris, list) else tuple(forms)
        if redirect not in registered:
            log.warning("onboarding: Composio registered eve without its callback")
            raise OnboardingError(COMPOSIO_NO_CALLBACK)
        log.info("onboarding: registered eve with Composio (%d callbacks)", len(registered))
        return _Client(client_id, registered)

    def _forget(self, client_id: str) -> None:
        self._gone.add(client_id)
        if self._registered is not None and self._registered.client_id == client_id:
            self._registered = None


def _expires_at(answer: Mapping[str, Any], access: str, now: float) -> float:
    """When the access token lapses, in epoch seconds: ``expires_in`` when the server says, else the token's own
    ``exp`` claim (read, not verified: it only times the refresh), else ``DEFAULT_TOKEN_SECS`` from now. A value
    that is not a sane lifetime (none, zero, past, absurdly far, not finite) is passed over, so the file always
    holds a finite number."""
    secs = answer.get("expires_in")
    if isinstance(secs, str):
        try:
            secs = float(secs.strip())
        except ValueError:
            secs = None
    if isinstance(secs, (int, float)) and not isinstance(secs, bool) and 0 < secs <= _MAX_TOKEN_SECS:
        return now + float(secs)
    exp = _jwt_exp(access)
    if exp is not None and now < exp <= now + _MAX_TOKEN_SECS:
        return exp
    return now + DEFAULT_TOKEN_SECS


def _jwt_exp(token: str) -> Optional[float]:
    parts = token.split(".")
    if len(parts) != 3:
        return None
    try:
        claims = json.loads(base64.urlsafe_b64decode(parts[1] + "=" * (-len(parts[1]) % 4)))
    except (ValueError, TypeError):
        return None
    return _num(claims.get("exp")) if isinstance(claims, dict) else None
