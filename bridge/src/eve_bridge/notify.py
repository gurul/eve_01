"""Notifications on the owner's phone through ntfy.sh (docs/notifications.md).

The owner asked on 2026-10-08 to "turn on notis for eve". With Telegram app-only (EVE_TELEGRAM=app,
telegram.MutedBotApi) nothing reaches Telegram, so nothing buzzed the phone when eve spoke unasked: a watch
alert, a chief nudge, a task finishing, a Claude relay reply. Apple's push is out (Eve Link is signed with a free
personal team), so eve posts to an ntfy.sh topic and the free ntfy app on the phone shows it.

* **What notifies.** Exactly what Telegram would have pushed: MutedBotApi hands every sendMessage, sendPhoto and
  sendDocument here (never an edit, "typing…", a draft, a reaction or a tap's answer). A reply to an Eve Link app
  turn goes to the app (TelegramInlet._to_app) and never reaches MutedBotApi, so it does not notify. A progress
  card (one Stop button) is skipped too: it says work started, and the line that says it finished notifies.
* **The topic.** "eve-" and 32 hex of HMAC-SHA256(link token, "eve notify"): the rendezvous's construction
  (stick_link.rendezvous_topic) under its own label, so the two topics differ. Nobody without the link token can
  name it; Eve Link computes the same in Settings (ios/EveLink/Shared/Rendezvous.swift, NotifyTopic).
* **Bursts.** A message waits COALESCE_SECS for others and one notification goes out with the newest message and
  "(+N more)"; while messages keep coming, at most one post per MIN_GAP_SECS, the rest folded into the next.
* **Never in the way.** ``offer`` returns at once; the post runs on a worker thread from a task. A failure is one
  warning line with the error's type, never the URL (it holds the topic) and never the message.
* **Privacy.** ntfy.sh is a public server: anyone who knows the topic can read what is posted on it. The topic is
  unguessable without the link token. EVE_NOTIFY_PREVIEW=0 posts "eve has a new message" instead of the
  words, so nothing eve says leaves the Mac. EVE_NOTIFY=0 turns notifications off.
"""
from __future__ import annotations

import asyncio
import hashlib
import hmac
import logging
import os
import re
import time
import urllib.error
import urllib.request
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Callable, Optional

log = logging.getLogger(__name__)

NOTIFY_URL = "https://ntfy.sh"
NOTIFY_LABEL = b"eve notify"
CLICK = "evelink://chat"          # a tap on the notification opens Eve Link's chat (LinkRoute.chat)
TITLE = "eve"
GENERIC = "eve has a new message"
TEST_LINE = "Notifications from eve work."
COALESCE_SECS = 3.0
MIN_GAP_SECS = 20.0
MAX_CHARS = 300
TIMEOUT_SECS = 10.0
APP_STORE = "https://apps.apple.com/app/ntfy/id1625396347"
_OFF = ("0", "false", "no", "off")
# The methods whose send Telegram would have pushed to the phone, and the field that holds the words.
NOTIFY_METHODS = {"sendMessage": "text", "sendPhoto": "caption", "sendDocument": "caption"}
_NO_WORDS = {"sendPhoto": "eve sent a picture", "sendDocument": "eve sent a file"}


def notify_topic(token: str) -> str:
    """The topic the phone computes the same way: "eve-" and 32 hex of HMAC-SHA256(token, "eve notify")."""
    return "eve-" + hmac.new(token.encode(), NOTIFY_LABEL, hashlib.sha256).hexdigest()[:32]


def token_path() -> Path:
    """Where the link token lives: next to the Mini App's ledger (miniapp.MiniApp, stick-link.json)."""
    return Path.home() / ".config" / "eve-bridge" / "stick-link.json"


def read_token(path: Optional[Path] = None) -> str:
    from .stick_link import LinkToken

    return LinkToken(path or token_path()).current()


@dataclass(frozen=True)
class NotifyConfig:
    enabled: bool = True
    base: str = NOTIFY_URL
    preview: bool = True


def configured(environ: Any = None) -> NotifyConfig:
    """EVE_NOTIFY (on unless "0"), EVE_NOTIFY_PREVIEW (on unless "0"), EVE_NOTIFY_URL (ntfy.sh).
    Only the app-only door asks (telegram.make_inlet): with Telegram on, Telegram itself notifies."""
    env = os.environ if environ is None else environ
    enabled = str(env.get("EVE_NOTIFY", "")).strip().lower() not in _OFF
    preview = str(env.get("EVE_NOTIFY_PREVIEW", "")).strip().lower() not in _OFF
    base = (env.get("EVE_NOTIFY_URL") or NOTIFY_URL).strip().rstrip("/")
    return NotifyConfig(enabled=enabled, base=base, preview=preview)


# ---- the words: plain text, short ---------------------------------------------------------------------------

_MD_LINK = re.compile(r"\[([^\]]+)\]\([^)]+\)")
_MD_MARKS = re.compile(r"(\*\*|__|~~|`+)")
_MD_HEADING = re.compile(r"^\s{0,3}#{1,6}\s+", re.MULTILINE)
_BLANKS = re.compile(r"\n\s*\n+")
_SPACES = re.compile(r"[ \t]+")


def plain_text(text: str) -> str:
    """A message as a notification shows it: OpenUI programs swapped for one line (link_chat.strip_openui),
    Telegram's HTML and leftover markdown gone, blank runs folded, cut to MAX_CHARS."""
    from . import telegram_format as fmt
    from .link_chat import strip_openui

    out = fmt.visible(strip_openui(text or ""))
    out = _MD_LINK.sub(r"\1", out)
    out = _MD_HEADING.sub("", out)
    out = _MD_MARKS.sub("", out)
    out = _SPACES.sub(" ", _BLANKS.sub("\n", out)).strip()
    if len(out) > MAX_CHARS:
        out = out[:MAX_CHARS - 1].rstrip() + "…"
    return out


def _post(url: str, body: bytes, headers: dict[str, str]) -> int:
    req = urllib.request.Request(url, data=body, method="POST",
                                 headers={"Content-Type": "text/plain; charset=utf-8", **headers})
    try:
        with urllib.request.urlopen(req, timeout=TIMEOUT_SECS) as r:  # noqa: S310 — an https ntfy base
            return r.status
    except urllib.error.HTTPError as e:
        return e.code


def post_once(token: str, body: str, base: str = NOTIFY_URL,
              post: Callable[[str, bytes, dict[str, str]], int] = _post) -> bool:
    """One notification on the token's topic. Blocking; False on any failure (logged by type, never raised)."""
    if not (token and base):
        return False
    try:
        status = post(f"{base}/{notify_topic(token)}", body.encode(), {"Title": TITLE, "Click": CLICK})
    except Exception as e:  # noqa: BLE001 — the type only: the message can carry the URL, which holds the topic
        log.warning("notify: could not post a notification (%s)", type(e).__name__)
        return False
    if status != 200:
        log.warning("notify: ntfy answered %s", status)
        return False
    return True


class Notifier:
    """What MutedBotApi hands its sends to. ``offer`` never blocks and never raises."""

    def __init__(self, token: Callable[[], str] = read_token, *, base: str = NOTIFY_URL, preview: bool = True,
                 post: Callable[[str, bytes, dict[str, str]], int] = _post,
                 clock: Callable[[], float] = time.monotonic,
                 coalesce_secs: float = COALESCE_SECS, gap_secs: float = MIN_GAP_SECS) -> None:
        self._token, self.base, self.preview, self._post = token, base, preview, post
        self._clock, self.coalesce_secs, self.gap_secs = clock, coalesce_secs, gap_secs
        self._last: str = ""
        self._count = 0
        self._posted_at: Optional[float] = None
        self._task: Optional[asyncio.Task] = None
        self.posts = 0                      # notifications posted, for the log and tests

    def offer_send(self, method: str, data: dict[str, Any]) -> None:
        """One Bot API send MutedBotApi swallowed. Only the sends Telegram would have pushed count."""
        field = NOTIFY_METHODS.get(method)
        if field is None:
            return
        try:
            if _is_progress_card(data):
                return
            words = data.get(field)
        except Exception as e:  # noqa: BLE001 — never the send's problem
            log.warning("notify: a notification was skipped (%s)", type(e).__name__)
            return
        self.offer(words if isinstance(words, str) and words.strip() else _NO_WORDS.get(method, GENERIC))

    def offer(self, text: str) -> None:
        try:
            line = plain_text(text) if self.preview else GENERIC
            if not line:
                return
            self._last = line
            self._count += 1
            if self._task is None or self._task.done():
                self._task = asyncio.get_running_loop().create_task(self._flush_soon())
        except RuntimeError:                # no running loop: nothing to post from (never the case in the inlet)
            log.debug("notify: no event loop; a notification was skipped")
        except Exception as e:  # noqa: BLE001 — a notification is never worth the send that offered it
            log.warning("notify: a notification was skipped (%s)", type(e).__name__)

    async def _flush_soon(self) -> None:
        try:
            while self._count:
                wait = self.coalesce_secs
                if self._posted_at is not None:
                    wait = max(wait, self._posted_at + self.gap_secs - self._clock())
                await asyncio.sleep(wait)
                last, count = self._last, self._count
                self._last, self._count = "", 0
                if not self.preview:
                    body = GENERIC if count == 1 else f"eve has {count} new messages"
                else:
                    body = last + (f" (+{count - 1} more)" if count > 1 else "")
                token = self._token() or ""
                self._posted_at = self._clock()
                if not token:
                    log.debug("notify: no link token yet (Eve Link not paired); not posted")
                    continue
                if await asyncio.to_thread(post_once, token, body, self.base, self._post):
                    self.posts += 1
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001
            log.warning("notify: the notifier stopped a round (%s)", type(e).__name__)

    async def close(self) -> None:
        task, self._task = self._task, None
        if task is not None and not task.done():
            task.cancel()
            try:
                await task
            except (asyncio.CancelledError, Exception):  # noqa: BLE001
                pass


def _is_progress_card(data: dict[str, Any]) -> bool:
    """A progress message (TelegramInlet._open_progress): one inline button, Stop. Work starting is not news."""
    from .telegram import STOP_BUTTON

    markup = data.get("reply_markup")
    rows = markup.get("inline_keyboard") if isinstance(markup, dict) else None
    if not (isinstance(rows, list) and len(rows) == 1 and isinstance(rows[0], list) and len(rows[0]) == 1):
        return False
    button = rows[0][0]
    return isinstance(button, dict) and button.get("text") == STOP_BUTTON and "callback_data" in button


def make_notifier(environ: Any = None) -> Optional[Notifier]:
    """The app-only door's notifier, or None when EVE_NOTIFY=0. The link token is read at each post, so a
    phone paired later (or a new token from /stick new) is picked up without a restart."""
    cfg = configured(environ)
    if not cfg.enabled or not cfg.base:
        log.info("notify: off (EVE_NOTIFY=0)")
        return None
    log.info("notify: on through ntfy (%s); the topic is Eve Link's (eve-bridge notify topic)",
             "previews" if cfg.preview else "no previews")
    return Notifier(base=cfg.base, preview=cfg.preview)


def setup_lines(token: str, base: str = NOTIFY_URL) -> list[str]:
    """What ``eve-bridge notify topic`` prints."""
    topic = notify_topic(token)
    return [topic, f"{base}/{topic}",
            "On the iPhone: install ntfy from the App Store, tap +, paste the topic, keep the default server, "
            "Subscribe. (Or Eve Link > Settings > Notifications > Copy.)"]
