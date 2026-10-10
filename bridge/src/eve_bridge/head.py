"""Head: eve turns its head when the owner asks — to any described pose, not a fixed menu.

The voice's backend model reads "look a bit up and to your left", "turn all the
way round", "look at the door" and picks the numbers; this module owns the
geometry and the wire. One absolute frame, robot-centred, the same one the
firmware uses:

    yaw    0 = straight ahead (toward the owner at the desk)
           + = the robot's own RIGHT, - = the robot's own LEFT   (body.cpp, bench-verified 2026-09-05)
           ±120 = as far as the neck turns: "behind you" is the nearest of the two
    pitch  45 = level, 5 = all the way down at the desk, 85 = up at the ceiling

The owner faces the robot, so "my left" is the robot's right. The backend
prompt says so; this module only ever speaks the robot's frame.

Poses go out as ``{"cmd":"look","yaw","pitch","hold"}``. The board echoes its
real pose on every camera frame (``"yaw"``/``"pitch"``), which ``observe``
records, so a relative move ("a bit more left") starts from where the head
actually is, not from where it was last told to go.

``look_around`` and ``find`` combine the head with scene.py: turn, let the
image settle, then describe (or search) a frame taken after the turn — a view
from before the move is never reported as the view at the new pose.
"""

from __future__ import annotations

import asyncio
import time
from typing import Any, Awaitable, Callable, Optional

YAW_MAX = 120                # body.cpp YAW_MAX
PITCH_MIN, PITCH_MAX = 5, 85  # body.cpp PITCH_MIN / PITCH_MAX
PITCH_LEVEL = 45             # body.cpp PITCH_LEVEL
CAMERA_HFOV_DEG = 66.0       # gaze.cpp kCameraHfovDeg (GC0308 lens assumption)
CAMERA_VFOV_DEG = CAMERA_HFOV_DEG * 3.0 / 4.0
DEFAULT_HOLD_SECS = 15.0
MAX_HOLD_SECS = 60.0         # the wire's hold is a uint16 in ms
SETTLE_SECS = 1.0            # head tween + a fresh frame after the move
SWEEP: tuple[tuple[int, int], ...] = ((-100, 50), (-50, 50), (0, 50), (50, 50), (100, 50))

Sender = Callable[[dict[str, Any]], Awaitable[bool]]

# A named pose → the move it means, in the robot's frame (negative yaw is its own left). The numbers are
# the ones the backend's prompt already uses ("a bit" ≈ 15°, "left" ≈ 70°, "all the way" 120), so a pose
# chosen by the fast path (typed_ask.py) lands where the backend would have put it.
POSE_MOVES: dict[str, dict[str, Any]] = {
    "left": {"yaw": -70, "relative": False}, "right": {"yaw": 70, "relative": False},
    "up": {"pitch": 70, "relative": False}, "down": {"pitch": 25, "relative": False},
    "center": {"yaw": 0, "pitch": PITCH_LEVEL, "relative": False},
    "far_left": {"yaw": -YAW_MAX, "relative": False}, "far_right": {"yaw": YAW_MAX, "relative": False},
    "bit_left": {"yaw": -15, "relative": True}, "bit_right": {"yaw": 15, "relative": True},
    "bit_up": {"pitch": 12, "relative": True}, "bit_down": {"pitch": -12, "relative": True},
    "desk": {"pitch": 10, "relative": False}, "ceiling": {"pitch": PITCH_MAX, "relative": False},
}


def clamp_pose(yaw: float, pitch: float) -> tuple[int, int, bool]:
    y = max(-YAW_MAX, min(YAW_MAX, int(round(yaw))))
    p = max(PITCH_MIN, min(PITCH_MAX, int(round(pitch))))
    return y, p, (y != int(round(yaw)) or p != int(round(pitch)))


def build_look_cmd(yaw: int, pitch: int, hold_ms: int) -> dict[str, Any]:
    return {"cmd": "look", "yaw": int(yaw), "pitch": int(pitch), "hold": int(max(0, min(65535, hold_ms)))}


def direction_word(yaw: float) -> str:
    """A spoken name for a yaw, in the robot's frame."""
    a = abs(yaw)
    side = "right" if yaw > 0 else "left"
    if a < 15:
        return "straight ahead"
    if a < 40:
        return f"a little to my {side}"
    if a < 80:
        return f"to my {side}"
    if a < 110:
        return f"far to my {side}"
    return f"behind me on my {side}"


def target_pose(pose_yaw: float, pose_pitch: float, x: float, y: float) -> tuple[int, int, bool]:
    """Absolute pose that centres a point seen at (x, y) in a frame taken at the given pose.
    x: -100 left edge .. 100 right edge; y: -100 top .. 100 bottom. Same mapping as
    gaze.cpp's host-face path: +x = +yaw, +y (down) = lower pitch."""
    yaw = pose_yaw + (x / 100.0) * (CAMERA_HFOV_DEG / 2.0)
    pitch = pose_pitch - (y / 100.0) * (CAMERA_VFOV_DEG / 2.0)
    return clamp_pose(yaw, pitch)


class Head:
    """The pose eve is holding, and the one command that changes it."""

    def __init__(
        self,
        send: Sender,
        connected: Callable[[], bool] = lambda: True,
        clock: Callable[[], float] = time.monotonic,
        sleep: Callable[[float], Awaitable[None]] = asyncio.sleep,
    ) -> None:
        self.send = send
        self.connected = connected
        self.clock = clock
        self.sleep = sleep
        self.yaw: float = 0.0
        self.pitch: float = float(PITCH_LEVEL)
        self.pose_at = float("-inf")     # when the board last echoed its pose
        # Told of every pose this class commands, with its hold: an asked-for pose (move_head, look_around,
        # find) owns the head, and the conversation's follower (follow.py) stands down for that long.
        self.on_move: Optional[Callable[[float], None]] = None

    def observe(self, yaw: Any, pitch: Any) -> None:
        """The pose echoed on a camera frame. Ignores anything that is not a number."""
        if isinstance(yaw, (int, float)) and not isinstance(yaw, bool):
            self.yaw = float(yaw)
            self.pose_at = self.clock()
        if isinstance(pitch, (int, float)) and not isinstance(pitch, bool):
            self.pitch = float(pitch)

    async def move(self, yaw: Optional[float] = None, pitch: Optional[float] = None,
                   relative: bool = False, hold_secs: float = DEFAULT_HOLD_SECS) -> dict[str, Any]:
        """Turn to a pose (absolute) or by an offset (relative). A missing axis keeps
        its current value. Hold 0 hands the head back to the robot's own behaviour."""
        if not self.connected():
            return {"ok": False, "reason": "the robot is not connected, so it cannot move"}
        base_y, base_p = self.yaw, self.pitch
        want_y = base_y if yaw is None else (base_y + yaw if relative else yaw)
        want_p = base_p if pitch is None else (base_p + pitch if relative else pitch)
        y, p, clamped = clamp_pose(want_y, want_p)
        hold = max(0.0, min(MAX_HOLD_SECS, float(hold_secs)))
        cmd = build_look_cmd(y, p, int(hold * 1000))
        ok = await self.send(cmd)
        if not ok:
            return {"ok": False, "reason": "the move did not reach the robot"}
        self.yaw, self.pitch = float(y), float(p)
        if self.on_move is not None:
            self.on_move(hold)
        out: dict[str, Any] = {"ok": True, "yaw": y, "pitch": p, "facing": direction_word(y), "hold_secs": hold}
        if clamped:
            out["note"] = (f"that is as far as the head goes (yaw ±{YAW_MAX}, pitch {PITCH_MIN}..{PITCH_MAX}); "
                           "it stopped at the limit")
        return out

    async def face_owner(self) -> dict[str, Any]:
        return await self.move(0, PITCH_LEVEL, hold_secs=3.0)


async def look_around(head: Head, scene: Any, stops: tuple[tuple[int, int], ...] = SWEEP,
                      settle: float = SETTLE_SECS) -> dict[str, Any]:
    """Pan left to right, describe each stop from a frame taken after the turn, then face the owner."""
    views: list[dict[str, Any]] = []
    for yaw, pitch in stops:
        r = await head.move(yaw, pitch, hold_secs=settle + 8.0)
        if not r["ok"]:
            return {**r, "views": views}
        moved_at = head.clock()
        await head.sleep(settle)
        v = await scene.look(fresh_secs=0.0, newer_than=moved_at)
        entry: dict[str, Any] = {"direction": direction_word(r["yaw"]), "yaw": r["yaw"]}
        if v.get("ok") and v.get("view"):
            entry["view"] = v["view"]
        else:
            entry["view"] = None
            entry["reason"] = v.get("reason", "no clear view")
            if "not connected" in str(v.get("reason", "")):
                await head.face_owner()
                return {"ok": False, "reason": v["reason"], "views": views}
        views.append(entry)
    await head.face_owner()
    return {"ok": True, "views": views}


async def find(head: Head, scene: Any, target: str, stops: tuple[tuple[int, int], ...] = SWEEP,
               settle: float = SETTLE_SECS, hold_secs: float = DEFAULT_HOLD_SECS) -> dict[str, Any]:
    """Look for ``target``: first where the head already points, then along the sweep.
    Centre on the first sighting and hold there."""
    target = " ".join(str(target).split())[:80]
    if not target:
        return {"ok": False, "reason": "no target given"}
    searched = 0
    plan: list[Optional[tuple[int, int]]] = [None, *stops]
    for stop in plan:
        if stop is not None:
            r = await head.move(stop[0], stop[1], hold_secs=settle + 8.0)
            if not r["ok"]:
                return {**r, "found": False}
        moved_at = head.clock()
        if stop is not None:
            await head.sleep(settle)
        loc = await scene.locate(target, newer_than=moved_at if stop is not None else None)
        searched += 1
        if not loc.get("ok"):
            if "not connected" in str(loc.get("reason", "")):
                return {"ok": False, "found": False, "reason": loc["reason"]}
            continue
        if loc.get("visible"):
            py = loc.get("yaw", head.yaw)
            pp = loc.get("pitch", head.pitch)
            ty, tp, _ = target_pose(py if py is not None else head.yaw, pp if pp is not None else head.pitch,
                                    float(loc.get("x", 0.0)), float(loc.get("y", 0.0)))
            r = await head.move(ty, tp, hold_secs=hold_secs)
            return {"ok": r["ok"], "found": True, "what": loc.get("what") or target,
                    "facing": direction_word(ty), "yaw": ty, "pitch": tp}
    await head.face_owner()
    return {"ok": True, "found": False, "searched_stops": searched,
            "reason": f"could not see {target} anywhere the head can turn"}
