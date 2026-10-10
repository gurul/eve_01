"""Eve's local math workspace, and the one lesson request shape shared by the voice tool and
``eve-bridge lesson``.

No hardware or third-party imports at startup: voice_agent and cli import this package when they
load, and server.py imports it too, so it must never import ``.server``.
"""

from __future__ import annotations

from typing import Any, Callable, Mapping, Optional

from .think_aloud import LISTEN_ACTIONS

LESSON_ACTIONS = ("open", "start", "ideas", "hint", "check", "step", "status", "recap", "end", *LISTEN_ACTIONS)
LESSON_MODES = ("learn", "help")
# Actions that can call the tutor model, so they can take a long time.
TUTOR_ACTIONS = ("start", "hint", "check", "step", "recap")
UNAVAILABLE = ("The learning workspace is unavailable. It is off (EVE_LEARNING=0) or its server "
               "did not start; check the bridge log.")
STALE_LESSON = "That saved lesson is no longer there. Open the learning window and choose a lesson."
# The learning server runs without the daemon (`eve-bridge learning`): nothing owns a microphone.
ROBOT_APP_NOT_RUNNING = ("Think out loud needs the eve robot app, and it is not running. "
                         "Start it with `eve-bridge daemon` or the launchd service.")
_LIMITS = {"topic": 200, "level": 100, "text": 20000}


def lesson_request(args: Any) -> dict[str, str]:
    """Validate and normalize a lesson request. Raise ValueError with a learner-safe sentence.

    Unknown keys are dropped, never forwarded."""
    if not isinstance(args, Mapping):
        raise ValueError("A lesson request needs an action.")
    action = str(args.get("action") or "").strip().lower()
    if action not in LESSON_ACTIONS:
        raise ValueError(f"Unknown lesson action {action!r}. Use one of: {', '.join(LESSON_ACTIONS)}.")
    out = {"action": action}
    mode = str(args.get("mode") or "").strip().lower()
    if mode and mode not in LESSON_MODES:
        raise ValueError("Mode must be learn or help.")
    if mode:
        out["mode"] = mode
    for key, limit in _LIMITS.items():
        value = args.get(key)
        if value is not None:
            out[key] = str(value)[:limit]
    return out


def run_lesson(voice: Optional[Callable[..., dict[str, Any]]], args: Any) -> dict[str, Any]:
    """Run one lesson action through ``LearningApp.voice``. Blocking: call it from asyncio.to_thread.

    User-level problems come back as ``{"ok": False, "reason": ...}``. Unexpected errors propagate."""
    if voice is None:
        return {"ok": False, "reason": UNAVAILABLE}
    try:
        request = lesson_request(args)
        return voice(**request)
    except KeyError:
        return {"ok": False, "reason": STALE_LESSON}
    except ValueError as exc:
        return {"ok": False, "reason": str(exc)}
