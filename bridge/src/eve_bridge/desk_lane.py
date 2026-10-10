"""The desk lane: keyword-decided clicks on the frontmost window, in front of Codex.

Every desktop task went to Codex's Computer Use: a model turn, then another, tens of seconds, for "switch to
week view" too. The fast lane that clicks such a request in about a second (fast_lane.py, lane_router.py) had
been wired only under the old planner, which Codex replaced. This puts lane_router.route back in the path, in
front of Codex, through app_reflex.ReflexFirstAgent:

    complete  every clause was clicked and seen to change something → eve says so; Codex never starts
    partial   something was clicked but not all of it, or not verified → Codex runs the task, told what was
              clicked, so it looks before it acts and never clicks the same thing blind (an unknown effect is
              never repeated: computah's rule)
    none / refused / unavailable → Codex runs the task exactly as before; the lane cost one AX snapshot

The router is the measured one (lane_router.py, fast_lane.LANE_FIRST_DEFAULT): keyword gate only, every word of
the request accounted for by one control's label, a question / another app / a long request refused before
any snapshot, every sensitive label and dialog a stop with zero input. On holdout it engaged on 23 of 82 cases
and was right on all 23, with no sensitive click. It never types and never presses a key.

The lane runs in the daemon (no worker process to start), on a thread, one task at a time. It needs the
daemon's Accessibility access; without it the snapshot is empty and every request goes to Codex, said once
in the log. ``EVE_DESK_LANE=0`` turns it off.
"""

from __future__ import annotations

import asyncio
import logging
import os
import threading
import time
from typing import Any, Callable, Optional

log = logging.getLogger(__name__)

ENV = "EVE_DESK_LANE"
DEFAULT: bool = True
MAX_GOAL_CHARS = 400


def enabled(environ: Any = None) -> bool:
    raw = str((os.environ if environ is None else environ).get(ENV, "") or "").strip().lower()
    if raw in ("0", "false", "no", "off"):
        return False
    if raw in ("1", "true", "yes", "on"):
        return True
    return DEFAULT


def _real_helpers() -> Any:
    """Helpers over the real desktop, configured as desktop_worker.main configures them."""
    import pyautogui

    from .desktop_helpers import Helpers

    pyautogui.FAILSAFE = True
    pyautogui.PAUSE = 0.05
    return Helpers(pyautogui)


def _trusted() -> bool:
    try:
        from ApplicationServices import AXIsProcessTrusted

        return bool(AXIsProcessTrusted())
    except Exception:  # noqa: BLE001 — no pyobjc, no lane
        return False


class DeskLane:
    """``await lane.attempt(goal)`` → lane_router.RouteResult as a dict, or None when the lane did not try."""

    def __init__(self, make_helpers: Callable[[], Any] = _real_helpers,
                 trusted: Callable[[], bool] = _trusted) -> None:
        self._make_helpers, self._trusted = make_helpers, trusted
        self._helpers: Any = None
        self._broken = ""                       # why the lane is off for this daemon's life, once known
        self._lock = threading.Lock()
        self._decisions: Any = None
        self._no_judge = False                  # the visual judge could not be built (no key); not retried

    def _attempt(self, goal: str) -> Optional[dict[str, Any]]:
        with self._lock:
            if self._broken:
                return None
            if self._helpers is None:
                if not self._trusted():
                    self._broken = "no Accessibility access for the daemon"
                    log.warning("desk lane: off — %s; desktop tasks go to Codex", self._broken)
                    return None
                try:
                    self._helpers = self._make_helpers()
                except Exception as e:  # noqa: BLE001 — a lane that cannot start leaves every task to Codex
                    self._broken = f"{type(e).__name__}: {e}"[:120]
                    log.warning("desk lane: off — %s; desktop tasks go to Codex", self._broken)
                    return None
            helpers = self._helpers
            lines: list[str] = []
            helpers.bind(lambda *values: lines.append(" ".join(str(v) for v in values)), lambda _image: None)
            helpers.begin()
            result = dict(helpers.lane_first(str(goal)[:MAX_GOAL_CHARS]))
            # Keep measured keyword successes cheap. The visual loop only sees a zero-input mismatch;
            # partial actions, dialogs and confirmation requests go directly to Codex.
            if result.get("status") == "none" and result.get("reason") in ("no_match", "ambiguous_target", "uncovered"):
                from . import decisions

                how = decisions.mode()
                if how != "off":
                    if self._decisions is None and not self._no_judge:
                        self._decisions = decisions.from_env()
                        self._no_judge = self._decisions is None      # said once in the log; not rebuilt per task
                    if self._decisions is not None:
                        judged = dict(helpers.decisions_first(str(goal)[:MAX_GOAL_CHARS], self._decisions,
                                                              dry_run=how == "shadow",
                                                              gates=getattr(self._decisions, "gates", None)))
                        if how == "shadow":
                            # Decided and logged (scores only, through helpers.log), never clicked: Codex
                            # still takes the task.
                            log.info("desk lane: visual judge shadow — %s", judged.get("reason") or "complete")
                        else:
                            result = judged
            result.setdefault("log", lines)
            return result

    async def attempt(self, goal: str) -> Optional[dict[str, Any]]:
        t0 = time.perf_counter()
        try:
            result = await asyncio.to_thread(self._attempt, goal)
        except Exception as e:  # noqa: BLE001 — never costs the task
            log.warning("desk lane: failed (%s); Codex takes it", type(e).__name__)
            return None
        if result is not None:
            # The outcome and the labels clicked, never the request's words.
            log.info("desk lane: %s%s in %.2f s%s", result.get("status"),
                     f" ({result['reason']})" if result.get("reason") else "", time.perf_counter() - t0,
                     f"; clicked {len(result.get('clicked') or [])}" if result.get("clicked") else "")
        return result


def codex_note(result: dict[str, Any]) -> str:
    """What Codex is told after a partial run, so it starts from the screen as the lane left it."""
    clicked = [str(c) for c in result.get("clicked") or []]
    if not clicked:
        return ""
    return ("\n\nBefore you started, eve's fast lane already clicked " + ", then ".join(repr(c) for c in clicked)
            + " in the frontmost window, and could not confirm the request is finished. Look at the screen first; "
              "do not click those again unless the screen shows they did nothing.")
