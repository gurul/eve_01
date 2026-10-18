"""The owner approves eve's connection to their Chrome from the phone.

Chrome asks "Allow remote debugging?" for every new connection to the owner's real Chrome (browser_lane.py,
attach mode) — the only lock between their logged-in browser and any program on the Mac. The owner is often
away from the desk (owner, 2026-09-23: "i won't be on computer to approve … text through telegram if it's
ok"), so the dialog's answer comes from them over Telegram and eve presses the button they chose:

* It is only ever used while eve ITSELF is connecting (``BrowserLane.connect``): the dialog it answers is
  the one eve's own connection raised, never a prompt some other program caused on its own.
* It acts only on a dialog that reads "Allow remote debugging?" and has an Allow button — Chrome's own.
* Allow is pressed only on the owner's clear yes (consent.approves: fail-closed). No, anything unclear, no
  answer in time, or no way to ask (Telegram off): Cancel is pressed — never a dialog left hanging — and the
  task goes to Codex, which already drives the owner's Chrome without this prompt.
* ``EVE_CHROME_ACCESS=allow`` is the owner's standing yes (owner, 2026-09-24: "Allow remote debugging
  should be allowed automatically don't ask me"): Allow is pressed at once, with no question. The two rules
  above still hold — only eve's own connection, only Chrome's own dialog. The default is ``ask``.

The buttons are Chrome web-UI controls: macOS Accessibility's AXPress presses them (a synthetic click does
not), through the AX API directly (pyobjc), with the daemon's own Accessibility permission.
"""

from __future__ import annotations

import asyncio
import logging
import time
from typing import Any, Awaitable, Callable, Optional

from . import consent

log = logging.getLogger(__name__)

DIALOG_TEXT = "Allow remote debugging?"
APPEAR_SECS = 10.0            # how long after eve starts connecting the dialog may take to show
POLL_SECS = 0.3
PRESS_TRIES = 5               # presses of one button, SETTLE_SECS apart, until the dialog is gone
SETTLE_SECS = 0.5             # after a press: Chrome's dialog ignores an AXPress in its first moments and still
                              # reports the press as done. Measured 2026-09-24 (Chrome, 10 fresh dialogs): a press
                              # ~0.25 s after the heading appeared did nothing 9 times of 10; a second press
                              # 0.5 s later dismissed it 5 of 5. So "pressed" is never believed — only "gone".
ASK_TIMEOUT_SECS = 100.0      # the owner's time to answer: inside browser_lane.CONSENT_TIMEOUT_MS (120 s), so a
                              # late yes never lands after the connection has given up
GONE_LINE = "Chrome's question was answered at the Mac, so you don't need to reply."
PRESS_FAILED_LINE = ("I couldn't press the button in Chrome: macOS may need to let eve control System Events "
                     "(System Settings → Privacy & Security → Automation). Codex will take this task.")
QUESTION = ("eve wants to control your Chrome (your logged-in browser) for a task: Chrome is asking "
            "\"Allow remote debugging?\". Allow it? yes / no")

# Chrome's dialog, as macOS Accessibility sees it (probed live, 2026-09-23): an AXHeading titled
# "Allow remote debugging?" and AXButtons "Allow" / "Cancel" in the same window, several AXGroups deep. An
# earlier AppleScript version looked for an AXStaticText and never matched the heading, so the phone was never
# asked; the AX API is read directly now (pyobjc, the daemon's own Accessibility permission).
MAX_NODES = 20000


def _ax():
    from ApplicationServices import (
        AXUIElementCopyAttributeValue,
        AXUIElementCreateApplication,
        AXUIElementPerformAction,
    )

    return AXUIElementCreateApplication, AXUIElementCopyAttributeValue, AXUIElementPerformAction


def _chrome_pids() -> list[int]:
    """Every running Chrome (two profiles' instances, say): the dialog is in whichever one eve connected to,
    and the first PID pgrep prints need not be it."""
    import subprocess

    out = subprocess.run(["pgrep", "-x", "Google Chrome"], capture_output=True, text=True).stdout.split()
    return [int(p) for p in out if p.isdigit()]


def _find_dialog(label: str = "") -> tuple[bool, Any]:
    """(dialog showing?, the button labelled ``label`` in that dialog's window, or None). Walks each Chrome's
    windows only as deep as needed; never presses anything."""
    create, get_attr, _ = _ax()

    def attr(el: Any, name: str) -> Any:
        err, value = get_attr(el, name, None)
        return value if err == 0 else None

    def label_of(el: Any) -> str:
        return str(attr(el, "AXTitle") or attr(el, "AXDescription") or attr(el, "AXValue") or "")

    for pid in _chrome_pids():
        for window in attr(create(pid), "AXWindows") or []:
            heading, button, count, stack = False, None, 0, [window]
            while stack and count < MAX_NODES:
                el = stack.pop()
                count += 1
                role = attr(el, "AXRole")
                text = label_of(el)
                if role in ("AXHeading", "AXStaticText") and text.strip() == DIALOG_TEXT:
                    heading = True
                elif role == "AXButton" and label and text.strip() == label and button is None:
                    button = el
                stack.extend(attr(el, "AXChildren") or [])
            if heading:
                return True, button
    return False, None


async def dialog_showing() -> bool:
    try:
        showing, _ = await asyncio.to_thread(_find_dialog)
    except Exception as e:  # noqa: BLE001 — no Accessibility, no pyobjc: "not showing", never a crash
        log.warning("chrome-consent: cannot read Chrome's dialog (%s)", type(e).__name__)
        return False
    return showing


async def press(label: str) -> bool:
    """Press Allow or Cancel on Chrome's own remote-debugging dialog. False when no such dialog is showing."""
    if label not in ("Allow", "Cancel"):
        raise ValueError(label)

    def act() -> bool:
        showing, button = _find_dialog(label)
        if not showing or button is None:
            return False
        _, _, perform = _ax()
        return perform(button, "AXPress") == 0

    try:
        return await asyncio.to_thread(act)
    except Exception as e:  # noqa: BLE001
        log.warning("chrome-consent: could not press %s (%s)", label, type(e).__name__)
        return False


def access_preference(environ: Any = None) -> str:
    """``allow`` (the owner's standing yes) or ``ask`` (the default). Anything else is ``ask``."""
    import os

    env = os.environ if environ is None else environ
    return "allow" if (env.get("EVE_CHROME_ACCESS") or "").strip().lower() == "allow" else "ask"


class ConsentBroker:
    """Answers the Chrome dialog raised by eve's own connection with the owner's Telegram yes or no.

    ``ask_owner(question) -> reply`` (Telegram), or None when there is no way to ask: then the dialog is
    cancelled — unless ``auto_allow`` (the owner's standing yes), which presses Allow without asking.
    ``showing``/``pressing`` are the Accessibility seams (tests fake them)."""

    def __init__(self, ask_owner: Optional[Callable[[str], Awaitable[str]]], *,
                 tell_owner: Optional[Callable[[str], Awaitable[None]]] = None,
                 showing: Callable[[], Awaitable[bool]] = dialog_showing,
                 pressing: Callable[[str], Awaitable[bool]] = press,
                 appear_secs: float = APPEAR_SECS, ask_timeout_secs: float = ASK_TIMEOUT_SECS,
                 poll_secs: float = POLL_SECS, settle_secs: float = SETTLE_SECS, auto_allow: bool = False) -> None:
        self._ask, self._showing, self._pressing = ask_owner, showing, pressing
        self._auto_allow = auto_allow
        self._tell = tell_owner
        self._appear, self._ask_timeout, self._poll = appear_secs, ask_timeout_secs, poll_secs
        self._settle = settle_secs
        self.last: str = ""                      # what happened last: allowed | declined | no_dialog | …

    async def answer_own_connection(self) -> str:
        """Called while eve's own connect is in flight. Waits for Chrome's dialog, asks the owner (or not, on
        the standing yes), presses the button. Returns allowed | auto_allowed | declined | unanswered |
        no_dialog | no_way_to_ask | answered_at_mac | press_failed | dialog_gone. Every outcome is logged: a
        silent early return once hid why the owner had to click at the Mac (2026-09-24)."""
        self.last = await self._answer()
        log.info("chrome-consent: %s", self.last)
        return self.last

    async def _answer(self) -> str:
        deadline = time.monotonic() + self._appear
        while not await self._showing():
            if time.monotonic() >= deadline:
                self.last = "no_dialog"          # already allowed on this connection, or Chrome did not ask
                return self.last
            await asyncio.sleep(self._poll)
        if self._auto_allow:
            outcome = await self._press_home("Allow")
            return "auto_allowed" if outcome == "pressed" else outcome
        if self._ask is None:
            await self._press_home("Cancel")
            self.last = "no_way_to_ask"
            return self.last
        asking = asyncio.ensure_future(self._ask(QUESTION))
        deadline = time.monotonic() + self._ask_timeout
        try:
            while not asking.done():
                if time.monotonic() >= deadline:
                    raise asyncio.TimeoutError
                await asyncio.sleep(self._poll)
                if not asking.done() and not await self._showing():
                    # Answered at the Mac (or the connection gave up): withdraw the phone question, so the
                    # owner's next text is not swallowed as its answer.
                    asking.cancel()
                    await asyncio.gather(asking, return_exceptions=True)
                    await self._say(GONE_LINE)
                    self.last = "answered_at_mac"
                    return self.last
            reply = asking.result()
        except asyncio.CancelledError:
            # connect() cancels this once its connection is done (BrowserLane.connect): withdraw the phone
            # question too, or the owner's next unrelated message is swallowed as its answer.
            if not asking.done():
                asking.cancel()
                await asyncio.gather(asking, return_exceptions=True)
            raise
        except (asyncio.TimeoutError, Exception) as e:  # noqa: BLE001 — no answer is a no
            log.info("chrome-consent: no answer from the owner (%s); cancelling", type(e).__name__)
            if not asking.done():
                asking.cancel()
                await asyncio.gather(asking, return_exceptions=True)
            await self._press_home("Cancel")
            self.last = "unanswered"
            return self.last
        if consent.approves(reply):
            outcome = await self._press_home("Allow")
            if outcome == "press_failed":
                await self._say(PRESS_FAILED_LINE)           # still up and we could not press it: say why
            self.last = "allowed" if outcome == "pressed" else outcome
        else:
            await self._press_home("Cancel")
            self.last = "declined"
        return self.last

    async def _press_home(self, label: str) -> str:
        """Press ``label`` until the dialog is gone. ``pressed``: gone after a press of ours; ``dialog_gone``:
        gone though no press landed (answered at the Mac, or the connection gave up); ``press_failed``: still
        up after PRESS_TRIES. A press is only counted once the dialog has gone (see SETTLE_SECS): the daemon
        logged ``auto_allowed`` at 15:16 on 2026-09-24 while the dialog stayed on screen for the owner to click."""
        for _ in range(PRESS_TRIES):
            pressed = await self._pressing(label)
            await asyncio.sleep(self._settle)
            if not await self._showing():
                return "pressed" if pressed else "dialog_gone"
        return "press_failed"

    async def _say(self, text: str) -> None:
        if self._tell is not None:
            try:
                await self._tell(text)
            except Exception:  # noqa: BLE001 — telling is best effort
                pass
