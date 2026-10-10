"""A watchdog for the daemon's event loop.

Every stall of the loop looks the same from the outside: the log goes quiet, the
robot sits still, then everything that queued up lands in one burst. The 2026-09-21
wake-word stall (heard 'hey eve' → 19 s of nothing → "Yeah?") and the 41 s
hang right after the serial port opened both had that shape, and the log could not
say which call held the loop, because the loop is what writes the log.

So a thread watches the loop from outside. The loop pets the watchdog every
quarter second with `call_later`; when the pets stop for longer than `threshold`,
the thread logs the loop thread's Python stack at WARNING — the one fact a stall
leaves nowhere else — and again every `repeat` seconds while it lasts. When the
loop comes back it logs how long it was gone.

`faulthandler` is registered on SIGUSR1 as well, for a stall this thread cannot
see (the GIL held by a C call): `kill -USR1 <pid>` dumps every thread to stderr,
which launchd points at the daemon log.
"""
from __future__ import annotations

import faulthandler
import logging
import signal
import sys
import threading
import time
import traceback
from typing import Any, Callable, Optional

log = logging.getLogger(__name__)

PET_SECS = 0.25          # how often the loop reports in
DEFAULT_THRESHOLD = 2.0  # silence longer than this is a stall
DEFAULT_REPEAT = 10.0    # while it lasts, log the stack again this often


def format_stack(thread_ident: int) -> str:
    """The Python stack of one thread, innermost call last (like a traceback)."""
    frame = sys._current_frames().get(thread_ident)  # noqa: SLF001 — the documented way to read another thread
    if frame is None:
        return "  (no frame for that thread)"
    return "".join(traceback.format_stack(frame)).rstrip()


class LoopWatchdog:
    """Watch an asyncio loop from a thread; log its stack when it stops ticking."""

    def __init__(self, loop: Any, threshold: float = DEFAULT_THRESHOLD, repeat: float = DEFAULT_REPEAT,
                 clock: Callable[[], float] = time.monotonic, poll: float = 0.5) -> None:
        self.loop = loop
        self.threshold = threshold
        self.repeat = repeat
        self._clock = clock
        self._poll = poll
        self._last_pet = clock()
        self._loop_thread: Optional[int] = None
        self._stop = threading.Event()
        self._thread: Optional[threading.Thread] = None
        self.stalls = 0              # stalls seen (tests, stats)
        self.longest = 0.0           # the longest one, seconds

    # -- loop side --
    def _pet(self) -> None:
        self._last_pet = self._clock()
        if self._loop_thread is None:
            self._loop_thread = threading.get_ident()
        if not self._stop.is_set():
            self.loop.call_later(PET_SECS, self._pet)

    # -- thread side --
    def _watch(self) -> None:
        reported_at: Optional[float] = None       # when the running stall was last logged
        stalled_since: Optional[float] = None
        while not self._stop.wait(self._poll):
            now = self._clock()
            quiet = now - self._last_pet
            if quiet > self.threshold:
                if stalled_since is None:
                    stalled_since = self._last_pet
                    self.stalls += 1
                if reported_at is None or now - reported_at >= self.repeat:
                    reported_at = now
                    stack = format_stack(self._loop_thread) if self._loop_thread is not None else "  (loop never ticked)"
                    log.warning("event loop stalled for %.1f s — the loop thread is at:\n%s", quiet, stack)
            elif stalled_since is not None:
                gone = self._last_pet - stalled_since
                self.longest = max(self.longest, gone)
                log.warning("event loop resumed after a %.1f s stall", gone)
                stalled_since = None
                reported_at = None

    def start(self) -> "LoopWatchdog":
        self.loop.call_soon(self._pet)
        self._thread = threading.Thread(target=self._watch, name="loop-watchdog", daemon=True)
        self._thread.start()
        return self

    def stop(self) -> None:
        self._stop.set()
        if self._thread is not None:
            self._thread.join(self._poll * 4)


def register_dump_signal(sig: int = signal.SIGUSR1) -> bool:
    """`kill -<sig> <pid>` dumps every thread's stack to stderr. False where signals are unsupported."""
    try:
        faulthandler.register(sig, all_threads=True, chain=True)
        return True
    except (AttributeError, ValueError, RuntimeError):
        return False
