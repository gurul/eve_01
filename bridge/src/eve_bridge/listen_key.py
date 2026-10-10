"""Listen key: mirror a held modifier on the Mac to the board.

The user's dictation app records while a global hotkey (Option by default)
is held. While that key is down the board should turn to face the user and
show its listening pose, and drop it on release. This module watches the
physical key; the daemon turns the transitions into wire messages:

    {"cmd":"listen","on":true}    key down
    {"cmd":"listen","on":false}   key up

Two halves, kept apart so the state machine is testable without a Mac:

* ``ListenKeyTracker`` — pure. Feed it the event flags and a clock; it
  reports a transition, or holds one back while the debounce window is open.
* ``start_listen_key`` — Quartz. A listen-only CGEventTap for flagsChanged
  runs on its own daemon thread and marshals flags to the asyncio loop.

key_tap.py (swipe-to-key) only taps Return and the arrow keys; it no
longer holds Option, so no synthetic modifier reaches this tap.

The tap needs macOS Input Monitoring for the daemon's python. Without it
CGEventTapCreate returns None; we log one warning naming the fix and the
feature is simply off — the daemon never crashes over it.
"""

from __future__ import annotations

import asyncio
import logging
import os
import sys
import threading
import time
from typing import Callable, Optional

from .key_tap import FLAG_ALTERNATE, FLAG_SECONDARY_FN

log = logging.getLogger(__name__)

# Minimum spacing between two transitions sent to the board. A flagsChanged
# storm (key bounce, an app re-posting modifiers) collapses to the state that
# holds once the window closes, so a quick tap still produces a clean
# down/up pair instead of losing its release.
DEBOUNCE_SECS = 0.15

# EVE_LISTEN_KEY → event-flag mask. "off" disables the watcher.
LISTEN_KEYS: dict[str, int] = {
    "option": FLAG_ALTERNATE,
    "fn": FLAG_SECONDARY_FN,
    "off": 0,
}

Stopper = Callable[[], None]


def default_listen_key() -> str:
    """Option on macOS; off elsewhere, where no tap can exist anyway."""
    return "option" if sys.platform == "darwin" else "off"


def configured_listen_key() -> str:
    name = (os.environ.get("EVE_LISTEN_KEY") or default_listen_key()).strip().lower()
    if name not in LISTEN_KEYS:
        fallback = default_listen_key()
        log.warning("listen key: unknown EVE_LISTEN_KEY=%r; using %s", name, fallback)
        return fallback
    return name


class ListenKeyTracker:
    """Turn a stream of modifier-flag snapshots into debounced down/up edges.

    ``feed`` returns True (down) or False (up) when the watched key changed
    state and the debounce window since the last reported edge has closed.
    An edge that lands inside the window is held as *pending*; ``due_in``
    says when it may go out and ``flush`` releases it. A pending edge that
    is cancelled by a return to the reported state (chatter) is dropped.
    """

    def __init__(self, key: Optional[str] = None, debounce: float = DEBOUNCE_SECS) -> None:
        self.key = key if key is not None else configured_listen_key()
        if self.key not in LISTEN_KEYS:
            raise ValueError(f"unknown listen key {self.key!r}")
        self.mask = LISTEN_KEYS[self.key]
        self.debounce = debounce
        self._down = False                 # last edge reported
        self._reported_at = float("-inf")  # first edge always passes
        self._pending: Optional[bool] = None

    @property
    def enabled(self) -> bool:
        return self.mask != 0

    @property
    def down(self) -> bool:
        return self._down

    def feed(self, flags: int, now: float) -> Optional[bool]:
        if not self.enabled:
            return None
        return self._settle(bool(flags & self.mask), now)

    def flush(self, now: float) -> Optional[bool]:
        """Release an edge held back by the debounce window, if it is due."""
        if self._pending is None:
            return None
        if now - self._reported_at < self.debounce:
            return None
        return self._settle(self._pending, now)

    def due_in(self, now: float) -> Optional[float]:
        """Seconds until the pending edge may be flushed; None when nothing is pending."""
        if self._pending is None:
            return None
        return max(0.0, self.debounce - (now - self._reported_at))

    def _settle(self, down: bool, now: float) -> Optional[bool]:
        if down == self._down:
            self._pending = None   # chatter resolved back to what we reported
            return None
        if now - self._reported_at < self.debounce:
            self._pending = down
            return None
        self._pending = None
        self._down = down
        self._reported_at = now
        return down


def start_listen_key(
    on_change: Callable[[bool], None],
    loop: asyncio.AbstractEventLoop,
    tracker: Optional[ListenKeyTracker] = None,
) -> Optional[Stopper]:
    """Watch the listen key and call ``on_change(down)`` on the loop.

    Returns a stopper, or None when the watcher cannot run (disabled, not
    macOS, Quartz missing, or Input Monitoring not granted). Every None path
    logs exactly once and never raises.
    """
    tracker = tracker if tracker is not None else ListenKeyTracker()
    if not tracker.enabled:
        log.info("listen key: disabled (EVE_LISTEN_KEY=off)")
        return None
    if sys.platform != "darwin":
        log.warning("listen key: only supported on macOS — board will not get listen events")
        return None
    try:
        import Quartz
    except ImportError:
        log.warning(
            "listen key: pyobjc-framework-Quartz not installed — board will not "
            "get listen events (pip install pyobjc-framework-Quartz)")
        return None

    def _feed(flags: int) -> None:
        # Loop thread. Emit now, or arm a flush for the end of the window.
        now = time.monotonic()
        change = tracker.feed(flags, now)
        if change is not None:
            on_change(change)
        delay = tracker.due_in(now)
        if delay is not None:
            loop.call_later(delay, _flush)

    def _flush() -> None:
        change = tracker.flush(time.monotonic())
        if change is not None:
            on_change(change)

    state: dict[str, object] = {"tap": None, "runloop": None}
    ready = threading.Event()

    def _callback(_proxy, etype, event, _refcon):  # Quartz calls this on the tap thread
        if etype in (Quartz.kCGEventTapDisabledByTimeout, Quartz.kCGEventTapDisabledByUserInput):
            # macOS disables a tap it thinks is slow; re-enable or go deaf.
            tap = state["tap"]
            if tap is not None:
                Quartz.CGEventTapEnable(tap, True)
            return event
        flags = int(Quartz.CGEventGetFlags(event))
        loop.call_soon_threadsafe(_feed, flags)
        return event

    def _thread() -> None:
        try:
            tap = Quartz.CGEventTapCreate(
                Quartz.kCGSessionEventTap,
                Quartz.kCGHeadInsertEventTap,
                Quartz.kCGEventTapOptionListenOnly,
                Quartz.CGEventMaskBit(Quartz.kCGEventFlagsChanged),
                _callback,
                None,
            )
            if tap is None:
                ready.set()
                return
            source = Quartz.CFMachPortCreateRunLoopSource(None, tap, 0)
            runloop = Quartz.CFRunLoopGetCurrent()
            Quartz.CFRunLoopAddSource(runloop, source, Quartz.kCFRunLoopCommonModes)
            Quartz.CGEventTapEnable(tap, True)
            state["tap"] = tap
            state["runloop"] = runloop
        except Exception:  # noqa: BLE001
            log.exception("listen key: tap setup failed")
            ready.set()
            return
        ready.set()
        Quartz.CFRunLoopRun()

    t = threading.Thread(target=_thread, name="listen-key-tap", daemon=True)
    t.start()
    ready.wait(timeout=5.0)
    if state["tap"] is None:
        log.warning(
            "listen key: could not create the event tap — grant Input Monitoring "
            "to %s in System Settings > Privacy & Security > Input Monitoring, "
            "then restart the daemon. Board will not get listen events.",
            os.path.realpath(sys.executable),
        )
        return None
    log.info("listen key: watching %s (EVE_LISTEN_KEY)", tracker.key)

    def stop() -> None:
        tap, runloop = state["tap"], state["runloop"]
        try:
            if tap is not None:
                Quartz.CGEventTapEnable(tap, False)
            if runloop is not None:
                Quartz.CFRunLoopStop(runloop)
        except Exception:  # noqa: BLE001
            log.debug("listen key: stop failed", exc_info=True)

    return stop
