"""Eve Link's door: the iPhone → daemon path (docs/action-button.md).

The owner asked on 2026-09-30 to "talk to it from anywhere". The Eve Link app on the iPhone (ios/EveLink)
carries each Action Button or Hold to talk turn to the daemon as a call (phone_call.py), through the Mini App's
Cloudflare tunnel (miniapp.py). The names here (``/stick``, ``stick-link.json``) are from the M5StickS3 button it
first carried, removed on 2026-10-07; they are kept so a paired phone keeps working. This file is what the daemon
adds for it:

* **The link token.** The Mini App's call proves the owner with Telegram's signed initData, which only Telegram's
  in-app browser has. The phone app instead holds a random token, made here on the owner's ``/stick`` and kept in
  ``stick-link.json`` (0600) next to the Mini App's ledger. ``/stick new`` replaces it, and the old one stops
  working at once: that is how a lost phone is cut off.
* **Its own door.** ``/api/stick`` is the call endpoint for the app, and takes only the token. A browser always
  sends an Origin on a WebSocket; the app sends none, so a request that has one is refused there, and the Mini
  App keeps ``/api/call`` exactly as it was.
* **Pairing without the token on the wire.** The quick tunnel's address changes every start (the owner chose,
  2026-09-30, a tap-to-update link over a fixed address). The pinned "eve: your apps." message gets a second
  button, *Update Eve Link*, edited to the new address with the Open button on every start. It opens
  ``https://<tunnel>/stick#t=<token>``: the token is in the fragment, which the browser never sends, and the
  page (static, no secrets) hands the address and the token to the app as ``evelink://pair?u=…&t=…``.
"""
from __future__ import annotations

import hashlib
import hmac
import json
import logging
import os
import re
import secrets
import time
import urllib.error
import urllib.request
from pathlib import Path
from typing import Any, Callable, Optional
from urllib.parse import quote

log = logging.getLogger(__name__)

PAIR_PATH = "/stick"
CALL_PATH = "/api/stick"
UPDATE_TEXT = "Update Eve Link"
PAIR_LINE = ("Tap the button to pair Eve Link with this Mac. After that you can talk to eve "
             "anywhere your phone has signal. When eve restarts, the pinned message's Update Eve Link button "
             "has the new address.")
NEW_LINE = "A new link token: the old one no longer works. Tap the button on the phone that should keep it."
OFF_LINE = "Eve Link needs the Mini App's tunnel, and it isn't up right now."


class LinkToken:
    """The one token the Eve Link app presents. Absent until the owner pairs."""

    def __init__(self, path: Path) -> None:
        self.path = Path(path)

    def current(self) -> str:
        try:
            token = json.loads(self.path.read_text()).get("token", "")
        except (OSError, ValueError, AttributeError):
            return ""
        return token if isinstance(token, str) and len(token) >= 32 else ""

    def _write(self, token: str) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        tmp = self.path.with_suffix(".tmp")
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w") as f:
            json.dump({"token": token, "created": int(time.time())}, f)
        os.replace(tmp, self.path)

    def ensure(self) -> str:
        return self.current() or self.rotate()

    def rotate(self) -> str:
        token = secrets.token_urlsafe(32)
        self._write(token)
        log.info("eve link: a new link token (the old one, if any, no longer works)")
        return token

    def check(self, presented: str) -> bool:
        want = self.current()
        return bool(want) and bool(presented) and hmac.compare_digest(want.encode(), presented.encode())


# ---- the rendezvous: the phone finds eve's new address by itself ----------------------------------------
#
# The quick tunnel's address changes every start, and on 2026-10-04 that broke the Action Button: the phone kept
# calling yesterday's address until the owner tapped Update Eve Link ("port isnt being held"). So the daemon also
# posts its address to an ntfy.sh topic (no account) that only the link token can name: the topic is an HMAC of
# a fixed label under the token, so it carries nothing about the token and nobody without the token can find it.
# The message is the tunnel's https address alone, which is no use without the token. The phone reads the topic
# at launch and whenever a call finds the old address gone (ios/EveLink/Shared/Rendezvous.swift).
# ntfy.sh keeps a message 12 hours, so the address is posted again every REPOST_SECS while it stays the same.
# EVE_LINK_RENDEZVOUS=0 turns it off (Update Eve Link still works); EVE_LINK_RENDEZVOUS_URL picks
# another ntfy server.
RENDEZVOUS_URL = "https://ntfy.sh"
RENDEZVOUS_LABEL = b"eve link rendezvous"
REPOST_SECS = 6 * 3600.0
RENDEZVOUS_TIMEOUT_SECS = 10.0


def rendezvous_topic(token: str) -> str:
    """The topic the phone computes the same way: "evelink-" and 32 hex of HMAC-SHA256(token, label)."""
    return "evelink-" + hmac.new(token.encode(), RENDEZVOUS_LABEL, hashlib.sha256).hexdigest()[:32]


def rendezvous_base(environ: Optional[dict] = None) -> str:
    """The ntfy server, or "" when the rendezvous is switched off."""
    env = os.environ if environ is None else environ
    if str(env.get("EVE_LINK_RENDEZVOUS", "")).strip().lower() in ("0", "false", "no", "off"):
        return ""
    return (env.get("EVE_LINK_RENDEZVOUS_URL") or RENDEZVOUS_URL).strip().rstrip("/")


def _post(url: str, body: bytes) -> int:
    req = urllib.request.Request(url, data=body, method="POST",
                                 headers={"Content-Type": "text/plain", "Title": "eve"})
    try:
        with urllib.request.urlopen(req, timeout=RENDEZVOUS_TIMEOUT_SECS) as r:  # noqa: S310 — a fixed https base
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def publish_address(public_url: str, token: str, base: str = RENDEZVOUS_URL,
                    post: Callable[[str, bytes], int] = _post) -> bool:
    """Post the tunnel's address to the token's topic. Blocking; False on any failure (logged, never raised)."""
    if not (public_url and token and base):
        return False
    try:
        status = post(f"{base}/{rendezvous_topic(token)}", public_url.rstrip("/").encode())
    except Exception as e:  # noqa: BLE001 — never raised: a reply that breaks http.client (BadStatusLine) or a base
        # with no scheme (ValueError) killed the announce loop (verification/Eve/Rendezvous.lean, 2026-10-04).
        # The type name only: the message can carry the topic, which names the token.
        log.warning("eve link: could not post the address for Eve Link (%s)", type(e).__name__)
        return False
    if status != 200:
        log.warning("eve link: the rendezvous answered %s", status)
        return False
    log.info("eve link: posted the address for Eve Link")   # the address is in the miniapp's own line
    return True


_ADDRESS_RE = re.compile(r"^https://[a-z0-9-]+\.trycloudflare\.com$")


def latest_address(token: str, base: str = RENDEZVOUS_URL, *, opener: Any = None) -> str:
    """The newest address the daemon posted to the rendezvous (what the phone reads), or "" when none is there or
    the read fails. Only a bare quick-tunnel origin is taken, as the phone does (Rendezvous.swift)."""
    if not token or not base:
        return ""
    url = f"{base.rstrip('/')}/{rendezvous_topic(token)}/json?poll=1&since=all"
    try:
        with (opener or urllib.request.urlopen)(url, timeout=RENDEZVOUS_TIMEOUT_SECS) as r:  # noqa: S310
            text = r.read().decode("utf-8", "replace")
    except (urllib.error.URLError, OSError, ValueError):
        return ""
    newest = ""
    for line in text.splitlines():
        try:
            event = json.loads(line)
        except ValueError:
            continue
        message = str(event.get("message") or "").strip().rstrip("/") if isinstance(event, dict) else ""
        if event.get("event") == "message" and _ADDRESS_RE.match(message):
            newest = message
    return newest


def pair_url(public_url: str, token: str) -> str:
    """The tap-to-update link. The token rides in the fragment, which never leaves the phone."""
    return f"{public_url.rstrip('/')}{PAIR_PATH}#t={quote(token, safe='')}"


# The pairing page: static, holds nothing secret, reads the fragment and offers the app's link. A button rather
# than an automatic jump: Telegram's in-app browser only opens another app from a tap.
PAIR_CSP = ("default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; "
            "base-uri 'none'; form-action 'none'; frame-ancestors 'none'")
PAIR_HEADERS = {"Content-Security-Policy": PAIR_CSP, "Referrer-Policy": "no-referrer",
                "X-Content-Type-Options": "nosniff"}                  # no-store comes with every reply
PAIR_PAGE = """<!doctype html>
<html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Eve Link</title>
<style>
  :root { color-scheme: light dark; --bg: #faf7f2; --fg: #1d1b18; --muted: #6b655c; --accent: #d9774b; }
  @media (prefers-color-scheme: dark) { :root { --bg: #161412; --fg: #f1ece4; --muted: #a39b8f; } }
  body { margin: 0; min-height: 100vh; display: grid; place-items: center; background: var(--bg); color: var(--fg);
         font: 17px/1.45 -apple-system, system-ui, sans-serif; }
  main { max-width: 22rem; padding: 24px 16px; text-align: center; }
  h1 { font-size: 1.5rem; margin: 0 0 .5rem; }
  p { color: var(--muted); margin: 0 0 1.5rem; }
  a.btn { display: inline-block; padding: 14px 26px; border-radius: 14px; background: var(--accent); color: #fff;
          font-weight: 600; text-decoration: none; }
  a.btn:focus-visible { outline: 3px solid var(--fg); outline-offset: 3px; }
</style></head>
<body><main>
  <h1>Eve Link</h1>
  <p id="say">Opening Eve Link.</p>
  <a class="btn" id="go" href="#">Open in Eve Link</a>
</main>
<script>
  var t = new URLSearchParams(location.hash.slice(1)).get("t") || "";
  history.replaceState(null, "", location.pathname);
  var go = document.getElementById("go");
  if (!t) {
    document.getElementById("say").textContent = "This link is missing its token. On the Mac, run eve-bridge link pair again.";
    go.remove();
  } else {
    go.href = "evelink://pair?u=" + encodeURIComponent(location.origin) + "&t=" + encodeURIComponent(t);
  }
</script>
</body></html>
"""
