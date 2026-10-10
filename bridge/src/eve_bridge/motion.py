"""Named motions the board runs by itself.

eve used to move by being told one absolute pose at a time. A dance is a
sequence, so a rhythm cost a model round trip per beat and came out stepped. The
measurement that started this: median 2.3 s from the owner's words to the head
moving, and the delegated model call was 1.86 s of it — five times everything
else combined (log study, 2026-09-11).

So the vocabulary moved onto the board. One JSON line names a rhythm and the
firmware runs it locally at its own tween rate (`motion.h`, `body.cpp`), which
means a whole dance costs one line instead of one line per beat, and the beats are
generated 25 times a second instead of arriving over USB.

    {"cmd":"move","kind":"osc","yaw_amp":25,"pitch_amp":12,"period_ms":900,
     "phase_deg":90,"cycles":8,"dwell_pct":8,"jitter_pct":10}

### Who decides what is safe

**The board does, and this module is only friendly.** Every number is passed
through `motion::admit()` on the firmware before a servo sees it: amplitude
shrinks to fit the travel left about the centre, the per-axis velocity budget and
the bout cap, and cycles are shed rather than beats shortened. A host bug, or a
model asking for 120 degrees at 10 Hz, gets something safe rather than obeyed —
verified against the shipped header, where that request comes back as 15 degrees
at 1.5 Hz.

The presets here are checked against the same header, so what this module
predicts is what the board runs. `dance` asks for 25 degrees rather than the 28
it once did, because 28 is what the board reduces it to.

### Amplitude yields, never the beat

When the owner asks for a faster dance, the rhythm is the salient thing. Speeding
a motion up shortens the period and the board then narrows the swing to stay
inside the velocity budget. A faster dance is a tighter dance, not a refused one.
"""

from __future__ import annotations

import logging
from typing import Any, Optional

log = logging.getLogger(__name__)

# 127 on an axis means "leave this one alone" (motion::KEEP).
KEEP = 127
MAX_KEYS = 8
SPEED_MIN, SPEED_MAX = 0.5, 2.0

# One bout each. Amplitudes in degrees about the centre, periods in ms.
# dwell_pct flat-tops the sine so the head pauses at each extreme the way a
# creature does; jitter_pct varies the swing cycle to cycle so it is not a
# metronome. Both are aliveness rather than capability: set them to zero and
# every motion still works.
OSC: dict[str, dict[str, int]] = {
    "sway": dict(yaw_amp=25, pitch_amp=0, period_ms=1600, cycles=3, dwell_pct=10, jitter_pct=8),
    # side to side and up and down at once, a quarter cycle apart, so the head
    # traces an ellipse rather than a diagonal line
    "dance": dict(yaw_amp=25, pitch_amp=12, period_ms=900, phase_deg=90, cycles=8,
                  dwell_pct=8, jitter_pct=10),
    "nod": dict(yaw_amp=0, pitch_amp=10, period_ms=700, cycles=3, dwell_pct=12, jitter_pct=6),
    "shake": dict(yaw_amp=18, pitch_amp=0, period_ms=650, cycles=3, dwell_pct=6, jitter_pct=8),
    "bounce": dict(yaw_amp=0, pitch_amp=8, period_ms=650, cycles=4, dwell_pct=10, jitter_pct=10),
    "wiggle": dict(yaw_amp=8, pitch_amp=0, period_ms=650, cycles=5, dwell_pct=0, jitter_pct=15),
    # yaw at half the pitch rate: a figure of eight
    "fig8": dict(yaw_amp=25, pitch_amp=10, period_ms=1600, pitch_period_ms=800, cycles=4,
                 dwell_pct=8, jitter_pct=8),
}

# Gestures whose SHAPE is the point, so they are keyframes rather than a rhythm:
# [at_ms, yaw, pitch, speed], 127 leaves an axis alone. Overshoot is hand-authored
# here, the way the firmware's own sequences already are.
KEYS: dict[str, list[list[int]]] = {
    "doubletake": [[0, 25, KEEP, 900], [180, -12, KEEP, 900], [340, 0, KEEP, 500]],
    "shrug": [[0, 14, 35, 800], [300, 14, 35, 200], [700, 0, 45, 250]],
    "tilt": [[0, 12, 53, 300]],
    "perk": [[0, KEEP, 70, 700]],
    "droop": [[0, KEEP, 33, 200]],
    "lean_peek": [[0, -70, 50, 200], [1800, -78, 58, 150], [3200, -78, 58, 150], [3400, 0, 45, 900]],
    "home": [[0, 0, 45, 400]],
}

def _clamp(v: float, lo: float, hi: float) -> float:
    return lo if v < lo else (hi if v > hi else v)


def osc_command(
    preset: Optional[str] = None,
    speed: float = 1.0,
    yaw_amp: Optional[int] = None,
    pitch_amp: Optional[int] = None,
    period_ms: Optional[int] = None,
    pitch_period_ms: Optional[int] = None,
    phase_deg: Optional[int] = None,
    cycles: Optional[int] = None,
    dwell_pct: Optional[int] = None,
    jitter_pct: Optional[int] = None,
    center_yaw: Optional[int] = None,
    center_pitch: Optional[int] = None,
) -> dict[str, Any]:
    """One `move` line for a rhythm. A preset is a starting point, not a cage.

    `speed` above 1 shortens the period, which is what "faster" means to a
    person. The swing then narrows on the board to stay inside the velocity
    budget: the beat is kept and the amplitude yields.
    """
    base = dict(OSC.get(preset or "", {}))
    if preset and preset not in OSC:
        raise ValueError(f"{preset!r} is not a rhythm; try {', '.join(sorted(OSC))}")
    for key, value in (("yaw_amp", yaw_amp), ("pitch_amp", pitch_amp), ("period_ms", period_ms),
                       ("pitch_period_ms", pitch_period_ms), ("phase_deg", phase_deg),
                       ("cycles", cycles), ("dwell_pct", dwell_pct), ("jitter_pct", jitter_pct)):
        if value is not None:
            base[key] = int(value)

    factor = _clamp(float(speed), SPEED_MIN, SPEED_MAX)
    if factor != 1.0:
        for key in ("period_ms", "pitch_period_ms"):
            if base.get(key):
                base[key] = int(round(base[key] / factor))

    out: dict[str, Any] = {"cmd": "move", "kind": "osc"}
    for key in ("yaw_amp", "pitch_amp", "period_ms", "pitch_period_ms", "phase_deg",
                "cycles", "dwell_pct", "jitter_pct"):
        if key in base:
            out[key] = int(base[key])
    # A centre is optional on purpose: without one the board centres the rhythm
    # on the pose the head is already holding, so a motion grows out of where it
    # is rather than snapping somewhere first.
    if center_yaw is not None:
        out["center_yaw"] = int(center_yaw)
    if center_pitch is not None:
        out["center_pitch"] = int(center_pitch)
    return out


def keys_command(preset: Optional[str] = None,
                 keys: Optional[list[list[int]]] = None) -> dict[str, Any]:
    """One `move` line for a keyframe gesture."""
    if keys is None:
        if preset not in KEYS:
            raise ValueError(f"{preset!r} is not a gesture; try {', '.join(sorted(KEYS))}")
        keys = [list(k) for k in KEYS[preset]]
    rows: list[list[int]] = []
    for k in keys[:MAX_KEYS]:
        if len(k) < 3:
            raise ValueError("a key is [at_ms, yaw, pitch] with an optional speed")
        at, yaw, pitch = int(k[0]), int(k[1]), int(k[2])
        speed = int(k[3]) if len(k) > 3 else 500
        rows.append([at, yaw, pitch, speed])
    if not rows:
        raise ValueError("no keys")
    return {"cmd": "move", "kind": "keys", "keys": rows}


def stop_command() -> dict[str, Any]:
    return {"cmd": "move", "kind": "stop"}


def predict(command: dict[str, Any]) -> dict[str, Any]:
    """What the host expects, before the board's own admission.

    Only ever used to describe a request and to sanity-check it in tests. The
    board is the authority and will reduce anything this gets wrong, which is why
    nothing here enforces a limit.
    """
    if command.get("kind") == "keys":
        keys = command.get("keys") or []
        last = max((int(k[0]) for k in keys), default=0)
        return {"kind": "keys", "keys": len(keys), "ms": last + 600}
    cycles = int(command.get("cycles", 4))
    period = int(command.get("period_ms", 900))
    yaw = int(command.get("yaw_amp", 0))
    pitch = int(command.get("pitch_amp", 0))
    freq = 1000.0 / period if period else 0.0
    return {
        "kind": "osc",
        "ms": cycles * period,
        "yaw_amp": yaw,
        "pitch_amp": pitch,
        "period_ms": period,
        # Peak speed of a sine at this amplitude and rate. The board's budget is
        # expressed the same way, so this is comparable to it.
        "peak_yaw_dps": round(2 * 3.141592653589793 * freq * yaw, 1),
        "peak_pitch_dps": round(2 * 3.141592653589793 * freq * pitch, 1),
    }


def describe(command: dict[str, Any]) -> str:
    """One line for a log or a terminal."""
    p = predict(command)
    if p["kind"] == "keys":
        return f"{p['keys']} keys over {p['ms']} ms"
    return (f"yaw {p['yaw_amp']}° pitch {p['pitch_amp']}° every {p['period_ms']} ms "
            f"for {p['ms']} ms (peak {p['peak_yaw_dps']}/{p['peak_pitch_dps']} °/s asked)")
