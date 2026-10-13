"""eve's browser on Steel: the self-hosted Steel browser API on this Mac, with no API key.

Owner, 2026-10-13: "replace browser with steel for eve" — "local steel". Steel (github.com/steel-dev/steel-browser,
Apache-2.0) is a browser API: it runs Chrome and hands out a Chrome DevTools websocket per session. The browser lane
already attaches to a Chrome over CDP (attach mode, own mode), so Steel is one more endpoint for the same lane:
``EVE_BROWSER_STEEL=1`` and the lane connects to a Steel session instead of eve's own Chrome. The plan executor, the
gate and the tab rules are unchanged (browser_lane.py).

    web task ──▶ BrowserLane._ensure ──▶ ensure_server()  (start steel-local.sh when nothing answers /v1/health)
                                     ──▶ start_session()  (POST /v1/sessions → websocketUrl ws://127.0.0.1:3000/)
                                     ──▶ Playwright connect_over_cdp(websocketUrl)

What this needs on the Mac (docs/steel.md):
- a checkout of steel-browser with its API built (``EVE_STEEL_DIR``, default ``~/Documents/personal/steel-browser``)
  and its ``steel-local.sh``, which binds 127.0.0.1 only and moves Chrome's DevTools port off 9222;
- Homebrew's ``node@22`` (Node 26 cannot build steel-browser's duckdb);
- the local timeout patch: upstream reports ``timeout: 0`` and Steel's CLI reads that as expired. eve itself never
  reads the timeout, so the lane works without the patch; only the ``steel`` CLI needs it.

Sign-ins: the server is started with ``CHROME_USER_DATA_DIR`` pointing at ``~/.config/eve-bridge/steel-profile``
(``EVE_STEEL_PROFILE`` moves it), so cookies stay between sessions and server restarts. Steel starts one session at
a time: a new session (from eve or from the ``steel`` CLI) replaces the running one.
"""

from __future__ import annotations

import json
import logging
import os
import subprocess
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Optional
from urllib.parse import urlsplit

log = logging.getLogger(__name__)

DEFAULT_URL = "http://127.0.0.1:3000"
DEFAULT_DIR = "~/Documents/personal/steel-browser"
DEFAULT_PROFILE = "~/.config/eve-bridge/steel-profile"
LAUNCHER = "steel-local.sh"
LOG_FILE = "~/Library/Logs/eve-steel.log"
START_SECS = 40.0              # a cold start: node, then Chrome; measured about 3 s on this Mac, 2026-10-13
HTTP_TIMEOUT = 10.0
LOOPBACK = frozenset({"127.0.0.1", "localhost", "::1"})


class SteelError(RuntimeError):
    """Steel cannot be reached or started. The message says what to do."""


@dataclass(frozen=True)
class SteelConfig:
    url: str = DEFAULT_URL
    steel_dir: Path = Path(DEFAULT_DIR).expanduser()
    profile: Path = Path(DEFAULT_PROFILE).expanduser()


def configured(environ: Any = None) -> SteelConfig:
    """``EVE_STEEL_URL`` (default http://127.0.0.1:3000; loopback only), ``EVE_STEEL_DIR`` (the steel-browser
    checkout eve starts the server from) and ``EVE_STEEL_PROFILE`` (the Chrome user data directory)."""
    env = os.environ if environ is None else environ
    url = (env.get("EVE_STEEL_URL") or "").strip().rstrip("/") or DEFAULT_URL
    return SteelConfig(url=url,
                       steel_dir=Path((env.get("EVE_STEEL_DIR") or "").strip() or DEFAULT_DIR).expanduser(),
                       profile=Path((env.get("EVE_STEEL_PROFILE") or "").strip() or DEFAULT_PROFILE).expanduser())


def _loopback(url: str) -> bool:
    return (urlsplit(url).hostname or "") in LOOPBACK


def _request(method: str, url: str, body: Optional[dict] = None, timeout: float = HTTP_TIMEOUT) -> dict:
    data = None if body is None else json.dumps(body).encode()
    req = urllib.request.Request(url, data=data, method=method, headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:   # noqa: S310 — loopback only, checked by callers
        raw = r.read()
    return json.loads(raw) if raw else {}


Http = Callable[..., dict]


def healthy(cfg: SteelConfig, http: Http = _request) -> bool:
    try:
        return http("GET", f"{cfg.url}/v1/health", timeout=2.0).get("status") == "ok"
    except (OSError, ValueError, urllib.error.URLError):
        return False


def launch_command(cfg: SteelConfig) -> list[str]:
    return [str(cfg.steel_dir / LAUNCHER)]


def ensure_server(cfg: SteelConfig, *, http: Http = _request, popen: Callable[..., Any] = subprocess.Popen,
                  sleep: Callable[[float], None] = time.sleep, clock: Callable[[], float] = time.monotonic,
                  wait_secs: float = START_SECS) -> None:
    """Steel is answering, or start it and wait until it is. Raises SteelError when it cannot be."""
    if not _loopback(cfg.url):
        raise SteelError(f"EVE_STEEL_URL must be on this Mac (127.0.0.1 or localhost), not {cfg.url}")
    if healthy(cfg, http):
        return
    launcher = cfg.steel_dir / LAUNCHER
    if not launcher.is_file():
        raise SteelError(f"Steel is not running and {launcher} is missing: see docs/steel.md")
    cfg.profile.mkdir(parents=True, exist_ok=True, mode=0o700)
    port = urlsplit(cfg.url).port or 3000
    env = {**os.environ, "HOST": "127.0.0.1", "PORT": str(port), "CHROME_USER_DATA_DIR": str(cfg.profile)}
    logfile = Path(LOG_FILE).expanduser()
    logfile.parent.mkdir(parents=True, exist_ok=True)
    with open(logfile, "ab") as out:
        # Its own session: the server outlives a daemon restart, like eve's own Chrome does.
        popen(launch_command(cfg), stdout=out, stderr=subprocess.STDOUT, stdin=subprocess.DEVNULL,
              env=env, cwd=str(cfg.steel_dir), start_new_session=True)
    log.info("steel: starting the local server (%s, profile %s)", cfg.url, cfg.profile)
    deadline = clock() + wait_secs
    while clock() < deadline:
        sleep(0.5)
        if healthy(cfg, http):
            log.info("steel: up at %s", cfg.url)
            return
    raise SteelError(f"Steel did not answer within {wait_secs:.0f} s: see {logfile}")


def start_session(cfg: SteelConfig, *, http: Http = _request, visible: bool = False) -> str:
    """A fresh Steel session; returns its CDP websocket URL. Starts the server first when needed.
    ``visible`` opens Chrome in a window on the Mac (signing in: ``eve-bridge steel-profile``)."""
    ensure_server(cfg, http=http)
    body: dict[str, Any] = {"blockAds": True}
    if visible:
        body["headless"] = False
    try:
        session = http("POST", f"{cfg.url}/v1/sessions", body)
    except (OSError, ValueError, urllib.error.URLError) as e:
        raise SteelError(f"Steel refused a new session: {e}") from None
    ws = str(session.get("websocketUrl") or "")
    if not ws.startswith(("ws://", "wss://")) or not _loopback(ws):
        raise SteelError(f"Steel answered without a local websocket URL ({ws or 'none'})")
    log.info("steel: session %s", str(session.get("id") or "?")[:8])
    return ws


def release(cfg: SteelConfig, *, http: Http = _request) -> None:
    """End the running session: Chrome closes and writes the profile. Quiet when Steel is gone."""
    try:
        http("POST", f"{cfg.url}/v1/sessions/release", {}, timeout=HTTP_TIMEOUT)
    except (OSError, ValueError, urllib.error.URLError):
        pass
