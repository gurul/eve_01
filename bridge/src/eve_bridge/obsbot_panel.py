"""The OBSBOT camera in Eve Link's Apps tab (docs/obsbot.md; owner, 2026-10-11: "build in an app into eve for
obsbot"): the camera's live picture, hold-to-turn arrows, zoom, center, sleep and wake, tracking and the AI mode.

The phone never talks to the camera. Every press comes through the tunnel to the daemon with the link token
(link_chat.py, ``POST /api/link/controls/obsbot``), and this panel drives OBSBOT Center's OSC server (obsbot.py).

Holding an arrow is Pika's hold-to-drive (pika.py): the app sends ``hold`` with the press's id about every 150 ms;
the first starts the camera's continuous USB turn (obsbot_usb.py: it turns until told to stop), and the rest only
renew it. The gimbal stops on ``stop``, when no beat came for HEARTBEAT_SECS, on ``close`` and when the
daemon stops. A late beat of a press that already ended is refused, so it never starts the camera turning again.

The picture: ffmpeg reads the webcam through AVFoundation, alongside OBSBOT Center (macOS lets both), and writes
MJPEG (measured 2026-10-11: first frame 0.45 s after ffmpeg starts, ~18 KB a frame at 640 px). One ffmpeg serves
every viewer (``Capture``) and stops a few seconds after the last one leaves, so the camera is read only while a
screen is open. The app asks for it over WebRTC first (``{"action": "rtc", "sdp"}``, pika_rtc.py's wire: straight
from this Mac, past the tunnel), and falls back to ``GET /api/link/controls/obsbot/stream``, Pika's multipart
format through the tunnel. Through the tunnel the frames came in bursts (gaps up to 0.7 s, 2026-10-11), as Pika's
did. The daemon needs the macOS camera permission; without it the screen says so in one line.

On by default when OBSBOT Center is installed; ``EVE_OBSBOT=0`` turns it off.
"""

from __future__ import annotations

import asyncio
import contextlib
import copy
import logging
import os
import re
import shutil
import time
from dataclasses import dataclass
from typing import Any, AsyncIterator, Callable, Optional

from . import obsbot, obsbot_usb, pika_rtc
from .pika import DEFAULT_STUN as STUN
from .pika import VIDEO_HEAD, _chunk, _json, split_jpegs, stamp_ms, video_part

log = logging.getLogger(__name__)

HEARTBEAT_SECS = 0.45                 # no beat from the app for this long: stop turning
HOLD_SPEED = obsbot_usb.TURN_SPEED    # the USB continuous turn's speed: slow enough to stop where you meant
VIDEO_FIRST_SECS = 6.0                # ffmpeg's first frame (0.45 s measured; the camera may be waking)
VIDEO_FRAME_SECS = 4.0                # no new frame for this long: the video ended
MAX_VIDEO_BUFFER = 1 << 20
VIDEO_WIDTH = 640
VIDEO_FPS = 15
LINGER_SECS = 3.0                     # the last viewer left: keep ffmpeg this long, for a reconnect
MAX_PEERS = 4                         # WebRTC viewers at once; past this the oldest is closed
DEVICE_RE = re.compile(r"\[(\d+)\] (.*OBSBOT.*)$")
ENDED_KEEP = 64                       # ended press ids remembered, to refuse their late beats

VIDEO_OFF_LINE = "The camera's picture isn't available. Is the Tiny 3 plugged in?"
VIDEO_DENIED_LINE = "The Mac didn't let eve read the camera. Allow it in System Settings, Privacy, Camera."
NO_FFMPEG_LINE = "The picture needs ffmpeg on the Mac (brew install ffmpeg)."


# ---- the models' camera (chat, app chat, voice; owner, 2026-10-11: "go for it build that") --------------------
# The models see tilt with + up and pan with + to the left, the way the picture moves; the camera's own pitch is +
# down (obsbot.py), so tilt = -pitch at this boundary and nowhere else.

OFF_REASON = "The OBSBOT camera is off on this Mac (no OBSBOT camera is plugged in, or EVE_OBSBOT=0)."
NOT_SET_UP_LINE = "The camera didn't load on this Mac. Run: eve-bridge obsbot setup"
ACTIONS = ("turn", "point", "center", "zoom", "wake", "sleep", "track_on", "track_off", "mode", "preset")
TURN_DEGREES = 20                     # a turn with no amount
BIT_DEGREES = 10                      # "a bit", "a little"
LOT_DEGREES = 45                      # "a lot", "all the way" is the range's end
ZOOM_STEP = 25                        # "zoom in" with no amount
TILT_RANGE = (-int(obsbot.PITCH_RANGE[1]), -int(obsbot.PITCH_RANGE[0]))

TOOLS: list[dict[str, Any]] = [
    {"type": "function", "name": "camera_status", "strict": True,
     "description": "The owner's OBSBOT webcam on the Mac right now: awake or asleep, where it points (pan "
                    "degrees left or right of straight ahead, tilt degrees up or down), the zoom in percent, "
                    "and its saved presets.",
     "parameters": {"type": "object", "additionalProperties": False, "required": [], "properties": {}}},
    {"type": "function", "name": "camera_control", "strict": True,
     "description": "Move the owner's OBSBOT webcam on the Mac: turn it (a direction and degrees from where it "
                    "points), point it (pan and tilt from straight ahead), center it, zoom (a level or a "
                    "change), wake it or put it to sleep, turn AI tracking on or off, set the AI mode, or go "
                    "to a saved preset. Pass null for what you don't use.",
     "parameters": {"type": "object", "additionalProperties": False,
                    "required": ["action", "direction", "degrees", "pan", "tilt", "zoom", "zoom_change",
                                 "mode", "preset"],
                    "properties": {
                        "action": {"type": "string", "enum": list(ACTIONS)},
                        "direction": {"type": ["string", "null"], "enum": ["left", "right", "up", "down", None],
                                      "description": "turn: which way, as the picture moves; or null."},
                        "degrees": {"type": ["integer", "null"],
                                    "description": f"turn: how far, 1-120; null for {TURN_DEGREES}. \"A bit\" "
                                                   f"is {BIT_DEGREES}, \"a lot\" {LOT_DEGREES}."},
                        "pan": {"type": ["integer", "null"],
                                "description": "point: degrees from straight ahead, -129 to 129, + is left; "
                                               "or null to keep it."},
                        "tilt": {"type": ["integer", "null"],
                                 "description": "point: degrees from level, -59 to 59, + is up; or null to keep "
                                                "it."},
                        "zoom": {"type": ["integer", "null"], "description": "zoom: 0-100 percent; or null."},
                        "zoom_change": {"type": ["integer", "null"],
                                        "description": f"zoom: a change such as {ZOOM_STEP} or -{ZOOM_STEP}, "
                                                       "when no level was asked; or null."},
                        "mode": {"type": ["string", "null"], "enum": [*obsbot.AI_MODES, None],
                                 "description": "mode: single follows one person, group frames everyone, voice "
                                                "follows who speaks, desk looks down at the desk, hand follows "
                                                "a hand, whiteboard frames a whiteboard; or null."},
                        "preset": {"type": ["integer", "null"],
                                   "description": "preset: 1, 2 or 3, saved in OBSBOT Center; or null."}}}},
]
TOOL_NAMES = tuple(t["name"] for t in TOOLS)

INSTRUCTIONS = f"""The owner's OBSBOT webcam, the Mac's camera (take_photo takes its pictures).
What you can do: camera_control turns it (a direction and degrees from where it points: a turn with no amount is
{TURN_DEGREES}°, "a bit" {BIT_DEGREES}°, "a lot" {LOT_DEGREES}°), points it (pan + left, tilt + up, from straight
ahead), centers it, zooms ("zoom in" is zoom_change {ZOOM_STEP}), wakes it or puts it to sleep, turns AI tracking on
or off, sets the AI mode and goes to a saved preset. camera_status reads where it points, the zoom and whether it
is awake.
Verify: turns, points, centering and zoom are read back from the camera: say where it ended up, in plain words
("a little to the left, level"). Tracking and the AI mode are not read back: say what you set.
To aim at something by name ("the door", "my desk"), you need to see the room first: take_photo, then turn toward
it, and say if you are guessing. While tracking is on, the camera follows a person and won't stay where it was
pointed: turn tracking off first when the owner wants it to hold still.
Limits: moving, zoom and waking always work. Tracking, the AI mode, presets and sleep are OBSBOT's own features and
work only while the OBSBOT Center app runs on the Mac; when one can't, say so in one line."""


def where(pan: Optional[int], pitch: Optional[int]) -> str:
    """Where the camera points, in the owner's words: "20° left, 10° up", "straight ahead"."""
    if pan is None or pitch is None:
        return "somewhere I couldn't read"
    tilt = -pitch
    parts = [] if abs(pan) <= 2 else [f"{abs(pan)}° {'left' if pan > 0 else 'right'}"]
    if abs(tilt) > 2:
        parts.append(f"{abs(tilt)}° {'up' if tilt > 0 else 'down'}")
    return ", ".join(parts) if parts else "straight ahead"


# ---- the code path: a message that is only a camera command names the camera -----------------------------------

@dataclass(frozen=True)
class Command:
    """A camera command the code understood."""
    action: str                         # status, turn, center, zoom, wake, sleep, track_on, track_off, preset
    direction: str = ""
    degrees: Optional[int] = None
    zoom: Optional[int] = None
    zoom_change: Optional[int] = None
    preset: Optional[int] = None


_LEAD = r"(?:(?:hey|ok|okay)\s+)?(?:eve\s+)?(?:(?:please|pls|can you|could you|would you)\s+)?"
_TAIL = r"(?:\s+(?:please|pls|thanks|thank you|eve|now))?"
_CAM = r"(?:the\s+|my\s+)?(?:camera|webcam|cam|obsbot)"
_TURN_VERB = r"(?:turn|move|pan|tilt|point|swing|rotate|look)"


def _turn(m: re.Match[str]) -> Optional[Command]:
    g = m.groupdict()
    deg = (BIT_DEGREES if g.get("bit") else LOT_DEGREES if g.get("lot")
           else int(g["deg"]) if g.get("deg") else TURN_DEGREES)
    if not 1 <= deg <= 120:
        return None                                     # an odd amount: the model gets it
    return Command("turn", direction=g["dir"], degrees=deg)


_AMOUNT_NAMED = (r"(?:\s+(?:by\s+)?(?:(?P<bit>a\s+(?:bit|little|touch|tad)|slightly)|(?P<lot>a\s+lot|a\s+bunch|way)"
                 r"|(?P<deg>\d{1,3})\s*(?:°|degrees?|deg)))?")
_DIR_NAMED = r"(?:to\s+the\s+)?(?P<dir>left|right|up|down)"


def _zoom_step(m: re.Match[str], sign: int) -> Command:
    g = m.groupdict()
    if g.get("all"):
        return Command("zoom", zoom=100 if sign > 0 else 0)
    return Command("zoom", zoom_change=sign * (ZOOM_STEP // 2 if g.get("bit") else ZOOM_STEP))


_ZOOM_AMOUNT = r"(?:\s+(?:(?P<bit>a\s+(?:bit|little|touch))|(?P<all>all\s+the\s+way)))?"
_PATTERNS: list[tuple[re.Pattern[str], Callable[[re.Match[str]], Optional[Command]]]] = [
    # "camera left", "turn the camera left a bit", "pan the webcam right 30 degrees", "camera to the left"
    (re.compile(rf"(?:{_TURN_VERB}\s+)?{_CAM}\s+{_DIR_NAMED}{_AMOUNT_NAMED}"), _turn),
    (re.compile(rf"{_TURN_VERB}\s+{_DIR_NAMED}{_AMOUNT_NAMED}\s+(?:on\s+|with\s+)?{_CAM}"), _turn),
    # "center the camera", "camera center", "camera straight ahead", "reset the camera"
    (re.compile(rf"(?:center|centre|recenter|reset|straighten)\s+{_CAM}"
                rf"|{_CAM}\s+(?:to\s+(?:the\s+)?)?(?:center|centre|straight\s+ahead|middle|home)"
                rf"|point\s+{_CAM}\s+straight(?:\s+ahead)?"),
     lambda m: Command("center")),
    # "zoom the camera in", "camera zoom in a bit", "zoom in on the camera" (the camera named), "camera zoom 50%"
    *[(re.compile(form.format(cam=_CAM, way=way, amount=_ZOOM_AMOUNT)), lambda m, sign=sign: _zoom_step(m, sign))
      for way, sign in (("in", 1), ("out", -1))
      for form in (r"zoom\s+{cam}\s+{way}{amount}", r"{cam}\s+zoom\s+{way}{amount}",
                   r"zoom\s+{way}{amount}\s+(?:on|with)\s+{cam}")],
    (re.compile(rf"unzoom\s+{_CAM}"), lambda m: Command("zoom", zoom=0)),
    (re.compile(rf"(?:set\s+)?{_CAM}\s+zoom\s+(?:to\s+)?(?P<z>\d{{1,3}})\s*%?|zoom\s+{_CAM}\s+to\s+(?P<z2>\d{{1,3}})\s*%?"),
     lambda m: Command("zoom", zoom=int(m.group("z") or m.group("z2")))),
    # "wake the camera", "camera on" (it wakes: OBSBOT's sleep is its off), "camera off", "put the camera to sleep"
    (re.compile(rf"wake\s+(?:up\s+)?{_CAM}(?:\s+up)?|(?:turn|switch)\s+on\s+{_CAM}|(?:turn|switch)\s+{_CAM}\s+on"
                rf"|{_CAM}\s+(?:on|wake(?:\s+up)?)"),
     lambda m: Command("wake")),
    (re.compile(rf"(?:turn|switch)\s+off\s+{_CAM}|(?:turn|switch)\s+{_CAM}\s+off|{_CAM}\s+(?:off|sleep)"
                rf"|put\s+{_CAM}\s+to\s+sleep"),
     lambda m: Command("sleep")),
    # "where is the camera pointing", "camera status"
    (re.compile(rf"{_CAM}\s+status|where(?:'s|\s+is)\s+{_CAM}\s+(?:pointing|pointed|looking|aimed)"
                rf"|is\s+{_CAM}\s+(?:on|awake|asleep)"),
     lambda m: Command("status")),
    # "camera track me", "camera follow me", "camera stop tracking", "stop the camera tracking"
    (re.compile(rf"{_CAM}\s+(?:track|follow)\s+me|(?:turn\s+on|start)\s+{_CAM}\s+tracking"
                rf"|{_CAM}\s+tracking\s+on"),
     lambda m: Command("track_on")),
    (re.compile(rf"{_CAM}\s+stop\s+(?:tracking|following)(?:\s+me)?|stop\s+{_CAM}\s+(?:tracking|following)(?:\s+me)?"
                rf"|(?:turn\s+off)\s+{_CAM}\s+tracking|{_CAM}\s+tracking\s+off"),
     lambda m: Command("track_off")),
    # "camera preset 2", "camera to preset 1"
    (re.compile(rf"{_CAM}\s+(?:to\s+)?preset\s+(?P<n>[123])|(?:go\s+to\s+)?preset\s+(?P<n2>[123])\s+on\s+{_CAM}"),
     lambda m: Command("preset", preset=int(m.group("n") or m.group("n2")))),
]


def _norm(text: str) -> str:
    t = (text or "").lower().replace("’", "'")
    return re.sub(r"\s+", " ", re.sub(r"[^\w%°'\s-]", " ", t)).strip()


def match(text: str) -> Optional[Command]:
    """A Command when the whole message is a camera command that names the camera, else None (the model answers).
    Anchored at both ends: "turn the camera to the door" leaves words over, so the model gets it. Reads no state."""
    t = _norm(text)
    if not t or len(t) > 100:
        return None
    for pattern, make in _PATTERNS:
        m = re.fullmatch(_LEAD + "(?:" + pattern.pattern + ")" + _TAIL, t)
        if m:
            cmd = make(m)
            if cmd is not None and cmd.zoom is not None and cmd.zoom > 100:
                return None
            return cmd
    return None


def enabled(environ: Any = None) -> bool:
    """On unless EVE_OBSBOT=0, while an OBSBOT camera is plugged in or OBSBOT Center is installed."""
    env = os.environ if environ is None else environ
    return env.get("EVE_OBSBOT", "1").strip() != "0" and (obsbot.APP_PATH.exists() or obsbot_usb.present())


def find_ffmpeg() -> Optional[str]:
    return shutil.which("ffmpeg") or next((p for p in ("/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg")
                                            if os.path.exists(p)), None)


class ObsbotPanel:
    """``panel(body)`` for the Apps tab, and ``stream(writer)`` for the picture."""

    def __init__(self, make: Callable[..., obsbot.Obsbot] = obsbot.Obsbot, ffmpeg: Optional[str] = None,
                 environ: Any = None, clock: Callable[[], float] = time.monotonic):
        self._make = make
        self._environ = environ
        self._ffmpeg = ffmpeg
        self._clock = clock
        self._cam: Optional[obsbot.Obsbot] = None      # reads, one at a time (its socket's replies are shared)
        self._lock = asyncio.Lock()
        self._hold: Optional[tuple[str, str]] = None   # (press id, direction) while the gimbal turns
        self._beat = 0.0
        self._ended: list[str] = []
        self._watch: Optional[asyncio.Task[None]] = None
        self._capture = Capture(self.frames)
        self._peers: dict[Any, Optional[asyncio.Task[None]]] = {}   # WebRTC peers -> their frame senders

    @property
    def available(self) -> bool:
        return enabled(self._environ)

    # -- the camera --
    async def _ready(self) -> Optional[str]:
        """None when the camera answers over USB; else the owner's line (no camera, no clang)."""
        if self._cam is not None and self._cam._ready:
            return None                                 # no lock: a beat must not wait behind a move's read-back
        async with self._lock:
            if self._cam is None:
                self._cam = self._make()
            return await asyncio.to_thread(self._cam.ensure)

    async def _read(self, fn: Callable[..., dict[str, Any]], *args: Any) -> dict[str, Any]:
        async with self._lock:
            res = await asyncio.to_thread(fn, *args)
        if not res.get("ok") and self._cam is not None:
            self._cam._ready = False                    # ask again next time: the camera may have been unplugged
        return res

    async def panel(self, body: dict[str, Any]) -> dict[str, Any]:
        action = str(body.get("action") or "status")
        if action == "close":                           # the picture ends with each viewer's own connection
            await self._stop_turning()
            return {"ok": True, "line": ""}
        if action == "rtc":
            return await self._rtc(body.get("sdp"))
        if action == "stop":
            await self._stop_turning(str(body.get("hold") or ""))
            return {"ok": True, "line": "", "stopped": True}
        why = await self._ready()
        if why:
            return {"ok": False, "line": why, "stopped": action == "hold"}
        cam = self._cam
        assert cam is not None
        if action == "hold":
            return await self._turn(str(body.get("hold") or ""), str(body.get("dir") or ""))
        if action == "status":
            res = await self._read(cam.status)
            return {**res, "modes": list(obsbot.AI_MODES)}
        if action == "move":
            return await self._read(cam.move, _num(body.get("pan")), _num(body.get("pitch")))
        if action == "center":
            return await self._read(cam.reset)
        if action == "zoom":
            level = _num(body.get("level"))
            if level is None:
                return {"ok": False, "line": "Zoom is 0 to 100."}
            return await self._read(cam.zoom, int(level))
        if action in ("wake", "sleep"):
            return await self._read(cam.wake, action == "wake")
        if action == "track":
            return await self._read(cam.track, bool(body.get("on")))
        if action == "mode":
            return await self._read(cam.mode, str(body.get("name") or ""))
        if action == "preset":
            n = _num(body.get("n"))
            return await self._read(cam.preset, int(n) if n is not None else 0)
        return {"ok": False, "line": "That isn't something the camera does."}

    # -- the models' tools (chat, app chat, voice) and the code path --
    def tools(self) -> list[dict[str, Any]]:
        return copy.deepcopy(TOOLS)

    def instructions(self) -> str:
        return INSTRUCTIONS

    def match(self, text: str) -> Optional[Command]:
        return match(text)

    async def handle(self, name: str, args: dict[str, Any]) -> dict[str, Any]:
        """A model's call. Every answer has ``ok`` and a plain ``line``; a move adds where the camera ended up."""
        if name not in TOOL_NAMES:
            return {"ok": False, "reason": f"unknown tool {name}"}
        why = await self._ready()
        if why:
            return {"ok": False, "reason": why}
        cam = self._cam
        assert cam is not None
        if name == "camera_status":
            st = await self._read(cam.status)
            if not st.get("ok"):
                return {"ok": False, "reason": st.get("line", "")}
            pan, pitch = st.get("pan"), st.get("pitch")
            saved = [p["name"] or f"preset {p['n']}" for p in st.get("presets", []) if p.get("set")]
            return {"ok": True, "awake": st.get("awake"), "pan": pan, "tilt": None if pitch is None else -pitch,
                    "zoom": st.get("zoom"), "presets": saved,
                    "line": f"{st.get('camera', 'The camera')} is {'asleep' if st.get('awake') is False else 'awake'}, "
                            f"pointing {where(pan, pitch)}, zoom {st.get('zoom', 0)}%."}
        return await self.control(**{k: args.get(k) for k in ("action", "direction", "degrees", "pan", "tilt",
                                                              "zoom", "zoom_change", "mode", "preset")})

    async def control(self, action: Any, direction: Any = None, degrees: Any = None, pan: Any = None,
                      tilt: Any = None, zoom: Any = None, zoom_change: Any = None, mode: Any = None,
                      preset: Any = None) -> dict[str, Any]:
        cam = self._cam
        assert cam is not None
        if action == "turn":
            if direction not in obsbot.DIRECTIONS:
                return {"ok": False, "reason": "turn needs a direction: left, right, up or down"}
            deg = _num(degrees)
            deg = TURN_DEGREES if deg is None else max(1.0, min(120.0, abs(deg)))
            now = await self._read(cam.position_reply)
            if not now.get("ok"):
                return {"ok": False, "reason": now.get("line", "")}
            p, q = now["pan"], now["pitch"]
            p, q = {"left": (p + deg, q), "right": (p - deg, q), "up": (p, q - deg), "down": (p, q + deg)}[direction]
            return self._moved(await self._read(cam.move, p, q), f"turn {direction}")
        if action == "point":
            pt, tl = _num(pan), _num(tilt)
            if pt is None and tl is None:
                return {"ok": False, "reason": "point needs pan, tilt or both"}
            return self._moved(await self._read(cam.move, pt, None if tl is None else -tl), "point")
        if action == "center":
            return self._moved(await self._read(cam.reset), "center")
        if action == "zoom":
            z, dz = _num(zoom), _num(zoom_change)
            if z is None and dz is None:
                return {"ok": False, "reason": "zoom needs zoom or zoom_change"}
            if z is None:
                now = await self._read(cam.zoom_reply)
                if not now.get("ok"):
                    return {"ok": False, "reason": now.get("line", "")}
                z = now["zoom"] + (dz or 0)
            res = await self._read(cam.zoom, int(max(0, min(100, z))))
            return {"ok": bool(res.get("ok")), "zoom": res.get("zoom"), "line": res.get("line", "")}
        if action in ("wake", "sleep"):
            res = await self._read(cam.wake, action == "wake")
            return {"ok": bool(res.get("ok")), "line": res.get("line", "")}
        if action in ("track_on", "track_off"):
            res = await self._read(cam.track, action == "track_on")
            on = action == "track_on" and res.get("ok")
            return {**res, "line": res["line"] + (" It follows a person now, not where it was pointed." if on else "")}
        if action == "mode":
            return await self._read(cam.mode, str(mode or ""))
        if action == "preset":
            n = _num(preset)
            return await self._read(cam.preset, int(n) if n is not None else 0)
        return {"ok": False, "reason": f"unknown action {action}"}

    @staticmethod
    def _moved(res: dict[str, Any], what: str) -> dict[str, Any]:
        """A move's answer for the model and the owner: where the camera ended up, tilt + up."""
        pan, pitch = res.get("pan"), res.get("pitch")
        if pan is None or pitch is None:
            return {"ok": False, "reason": res.get("line", f"the camera didn't confirm the {what}")}
        line = f"Camera pointing {where(pan, pitch)}."
        if not res.get("ok"):
            line = f"The camera stopped at {where(pan, pitch)} (the end of its range, or it is asleep)."
        return {"ok": bool(res.get("ok")), "pan": pan, "tilt": -pitch, "line": line}

    async def run(self, cmd: Command) -> str:
        """A code-path command (match): the line for the owner."""
        why = await self._ready()
        if why:
            return why
        if cmd.action == "status":
            out = await self.handle("camera_status", {})
        else:
            out = await self.control(cmd.action, direction=cmd.direction or None, degrees=cmd.degrees,
                                     zoom=cmd.zoom, zoom_change=cmd.zoom_change, preset=cmd.preset)
        log.info("obsbot: by code, %s -> %s", cmd.action, "done" if out.get("ok") else "refused")
        return str(out.get("line") or out.get("reason") or "")

    # -- hold to turn --
    async def _turn(self, hold: str, direction: str) -> dict[str, Any]:
        if not hold or direction not in obsbot.DIRECTIONS:
            return {"ok": False, "line": "Hold an arrow.", "stopped": True}
        if hold in self._ended:
            return {"ok": False, "line": "", "stopped": True}  # a late beat of a press that ended
        cam = self._cam
        assert cam is not None
        self._beat = self._clock()
        if self._hold != (hold, direction):
            if self._hold is not None:
                self._end(self._hold[0])
            first = self._hold is None
            self._hold = (hold, direction)
            if first and not await asyncio.to_thread(cam.usb.wake):   # a sleeping camera ignores the turn
                self._hold = None
                self._end(hold)
                return {"ok": False, "line": cam.usb.why or "The camera is asleep and didn't wake.", "stopped": True}
            if self._hold != (hold, direction):
                # The wake takes up to about 7 s and the press ended (or another replaced it) meanwhile: the
                # stop already went out, and a turn now would run the gimbal to the end of its range.
                return {"ok": False, "line": "", "stopped": True}
            # a new direction replaces the old turn in one request; later beats only renew it
            await asyncio.to_thread(cam.usb.turn, direction, HOLD_SPEED)
            self._beat = self._clock()
        if self._watch is None or self._watch.done():
            self._watch = asyncio.ensure_future(self._watchdog())
        return {"ok": True, "line": ""}

    def _end(self, hold: str) -> None:
        if hold and hold not in self._ended:
            self._ended.append(hold)
            del self._ended[:-ENDED_KEEP]

    async def _stop_turning(self, hold: str = "") -> None:
        """Stop every direction now (a stop is never queued behind a read), and end the press."""
        if self._hold is not None:
            self._end(self._hold[0])
        self._end(hold)
        self._hold = None
        if self._cam is not None:
            await asyncio.to_thread(self._cam.usb.turn, "stop")

    async def _watchdog(self) -> None:
        while self._hold is not None:
            await asyncio.sleep(0.1)
            if self._hold is not None and self._clock() - self._beat > HEARTBEAT_SECS:
                log.info("obsbot: no beat for %.2f s, stopping the gimbal", HEARTBEAT_SECS)
                await self._stop_turning()

    # -- the picture --
    async def _device(self, ffmpeg: str) -> Optional[str]:
        """The Tiny 3's AVFoundation index, by name: the list's order changes when cameras come and go."""
        proc = await asyncio.create_subprocess_exec(
            ffmpeg, "-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", "",
            stdin=asyncio.subprocess.DEVNULL, stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.PIPE)
        _, err = await proc.communicate()
        text = err.decode("utf-8", "replace")
        for line in text.splitlines():
            if "audio devices" in line:
                break
            m = DEVICE_RE.search(line)
            if m:
                return m.group(1)
        log.info("obsbot: no OBSBOT camera in ffmpeg's list (%s)", " | ".join(text.splitlines()[-6:])[-300:])
        return None

    async def frames(self) -> AsyncIterator[bytes]:
        """The webcam as whole JPEGs, the newest only. Raises LookupError with the owner's line when it can't start."""
        ffmpeg = self._ffmpeg or find_ffmpeg()
        if not ffmpeg:
            raise LookupError(NO_FFMPEG_LINE)
        index = await self._device(ffmpeg)
        if index is None:
            raise LookupError(VIDEO_OFF_LINE)
        proc = await asyncio.create_subprocess_exec(
            ffmpeg, "-hide_banner", "-loglevel", "error", "-f", "avfoundation", "-pixel_format", "nv12",
            "-framerate", "30", "-video_size", "1280x720", "-i", f"{index}:none",
            "-vf", f"fps={VIDEO_FPS},scale={VIDEO_WIDTH}:-2", "-q:v", "7", "-f", "mjpeg", "pipe:1",
            stdin=asyncio.subprocess.DEVNULL, stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        assert proc.stdout is not None and proc.stderr is not None
        buf, first = b"", True
        try:
            while True:
                try:
                    data = await asyncio.wait_for(proc.stdout.read(65536),
                                                  VIDEO_FIRST_SECS if first else VIDEO_FRAME_SECS)
                except asyncio.TimeoutError:
                    if first:
                        log.info("obsbot: no frame from ffmpeg in %.0f s", VIDEO_FIRST_SECS)
                        raise LookupError(VIDEO_OFF_LINE) from None
                    return
                if not data:
                    if first:
                        err = (await proc.stderr.read()).decode("utf-8", "replace").lower()
                        log.info("obsbot: ffmpeg ended before a frame (%s)", err.strip()[-160:])
                        raise LookupError(VIDEO_DENIED_LINE if "not authorized" in err or "permission" in err
                                          or "access" in err else VIDEO_OFF_LINE)
                    return
                jpegs, buf = split_jpegs(buf + data)
                if len(buf) > MAX_VIDEO_BUFFER:
                    buf = b""
                if jpegs:
                    first = False
                    yield jpegs[-1]
        finally:
            if proc.returncode is None:
                with contextlib.suppress(ProcessLookupError):
                    proc.kill()
                with contextlib.suppress(Exception):
                    await proc.wait()

    async def stream(self, writer: asyncio.StreamWriter) -> None:
        """The picture to the app as multipart JPEGs (pika.py's format) through the tunnel: WebRTC's fallback. Any
        number of viewers share one capture."""
        watch = self._capture.watch()
        try:
            try:
                first = await anext(watch)
            except LookupError as e:
                await _json(writer, 503, {"ok": False, "line": str(e)})
                return
            writer.write(VIDEO_HEAD)
            await writer.drain()
            log.info("obsbot: picture on (tunnel; %d watching)", self._capture.viewers)
            await _chunk(writer, video_part(first))
            async for jpeg in watch:
                await _chunk(writer, video_part(jpeg))
        except LookupError:
            with contextlib.suppress(Exception):
                await _chunk(writer, b"")
        except (OSError, ConnectionError):
            pass                                        # the app hung up
        finally:
            await watch.aclose()
            log.info("obsbot: picture off (tunnel; %d watching)", self._capture.viewers)

    # -- the picture over WebRTC (pika_rtc.py's wire: one data channel "video", stamp + JPEG per message) --
    async def _rtc(self, sdp: Any) -> dict[str, Any]:
        if not isinstance(sdp, str) or not sdp or len(sdp) > pika_rtc.MAX_SDP_CHARS or not pika_rtc.available():
            return {"ok": False, "rtc": False, "line": ""}
        try:
            answer = await asyncio.wait_for(self._answer(sdp), 8.0)
        except Exception as e:                          # aiortc raises its own kinds; the tunnel still works
            log.info("obsbot: webrtc answer failed (%s)", type(e).__name__)
            return {"ok": False, "rtc": False, "line": ""}
        return {"ok": True, "rtc": True, "sdp": answer, "line": ""}

    async def _answer(self, offer_sdp: str) -> str:
        from aiortc import RTCConfiguration, RTCIceServer, RTCPeerConnection, RTCSessionDescription

        while len(self._peers) >= MAX_PEERS:            # the oldest goes: a phone that vanished without closing
            await self._close_peer(next(iter(self._peers)))
        pc = RTCPeerConnection(RTCConfiguration(iceServers=[RTCIceServer(urls=STUN)]))
        self._peers[pc] = None

        @pc.on("datachannel")
        def _channel(channel: Any) -> None:
            if channel.label != "video" or pc not in self._peers:
                channel.close()
                return
            old = self._peers.get(pc)
            if old is not None:
                old.cancel()
            self._peers[pc] = asyncio.ensure_future(self._send(pc, channel))

        @pc.on("connectionstatechange")
        async def _state() -> None:
            if pc.connectionState in ("failed", "closed"):
                await self._close_peer(pc)

        try:
            await pc.setRemoteDescription(RTCSessionDescription(sdp=offer_sdp, type="offer"))
            await pc.setLocalDescription(await pc.createAnswer())     # gathers every candidate first
        except Exception:
            await self._close_peer(pc)
            raise
        return pc.localDescription.sdp

    async def _send(self, pc: Any, channel: Any) -> None:
        """The shared capture's frames into one peer's channel until it closes; a lost frame is skipped."""
        if channel.readyState != "open":
            opened = asyncio.Event()
            channel.on("open", opened.set)
            try:
                await asyncio.wait_for(opened.wait(), pika_rtc.OPEN_SECS)
            except asyncio.TimeoutError:
                return
        while channel.readyState == "open" and pc in self._peers:
            watch = self._capture.watch()
            log.info("obsbot: picture on (webrtc; %d watching)", self._capture.viewers + 1)
            try:
                async for jpeg in watch:
                    if channel.readyState != "open":
                        break
                    if len(jpeg) > pika_rtc.MAX_JPEG or channel.bufferedAmount > pika_rtc.BUFFER_LIMIT:
                        continue
                    channel.send(pika_rtc.frame_message(stamp_ms(), jpeg))
            except LookupError as e:
                if channel.readyState == "open":
                    channel.send(str(e))
            finally:
                await watch.aclose()
                log.info("obsbot: picture off (webrtc; %d watching)", self._capture.viewers)
            if channel.readyState == "open":
                await asyncio.sleep(pika_rtc.RETRY_SECS)

    async def _close_peer(self, pc: Any) -> None:
        if pc not in self._peers:
            return
        task = self._peers.pop(pc)
        if task is not None and task is not asyncio.current_task() and not task.done():
            task.cancel()
            with contextlib.suppress(BaseException):
                await task
        with contextlib.suppress(Exception):
            await pc.close()

    async def close(self) -> None:
        await self._stop_turning()
        for pc in list(self._peers):
            await self._close_peer(pc)
        await self._capture.stop()
        if self._cam is not None:
            self._cam.close()


class Capture:
    """One ffmpeg for every viewer. It starts with the first viewer, keeps only the newest frame, and stops
    LINGER_SECS after the last one leaves (a phone that reconnects reuses it). Viewers never end each other: on
    2026-10-11 a single-viewer rule had two open screens (the phone and a simulator) take the camera from each
    other every 3 s."""

    def __init__(self, frames: Callable[[], AsyncIterator[bytes]]):
        self._frames = frames
        self.jpeg: Optional[bytes] = None
        self.seq = 0
        self.error: Optional[str] = None
        self.viewers = 0
        self._task: Optional[asyncio.Task[None]] = None
        self._linger: Optional[asyncio.Task[None]] = None
        self._changed = asyncio.Event()

    @property
    def running(self) -> bool:
        return self._task is not None and not self._task.done()

    def _publish(self) -> None:
        self._changed.set()
        self._changed = asyncio.Event()

    async def _run(self) -> None:
        line = VIDEO_OFF_LINE
        try:
            async for jpeg in self._frames():
                self.jpeg, self.seq, self.error = jpeg, self.seq + 1, None
                self._publish()
        except LookupError as e:
            line = str(e)
        finally:
            self.jpeg, self.error = None, line
            self._publish()

    async def watch(self) -> AsyncIterator[bytes]:
        """The newest frame each time there is a new one. Raises LookupError with the owner's line when the camera
        can't start, ends or goes quiet."""
        self.viewers += 1
        if self._linger is not None:
            self._linger.cancel()
            self._linger = None
        if not self.running:
            self.jpeg, self.error = None, None
            self._task = asyncio.ensure_future(self._run())
        seen = self.seq - 1 if self.jpeg is not None else self.seq
        first = True
        try:
            while True:
                if self.seq != seen and self.jpeg is not None:
                    seen = self.seq
                    first = False
                    yield self.jpeg
                    continue
                if self.error is not None:
                    raise LookupError(self.error)
                changed = self._changed
                try:
                    await asyncio.wait_for(changed.wait(), VIDEO_FIRST_SECS if first else VIDEO_FRAME_SECS)
                except asyncio.TimeoutError:
                    raise LookupError(VIDEO_OFF_LINE) from None
        finally:
            self.viewers -= 1
            if self.viewers == 0 and self.running:
                self._linger = asyncio.ensure_future(self._stop_later())

    async def _stop_later(self) -> None:
        await asyncio.sleep(LINGER_SECS)
        if self.viewers == 0:
            await self.stop()

    async def stop(self) -> None:
        task, self._task = self._task, None
        if task is not None and not task.done():
            task.cancel()                               # frames()' finally kills ffmpeg
            with contextlib.suppress(BaseException):
                await task
        self.jpeg = None


def _num(v: Any) -> Optional[float]:
    if isinstance(v, bool) or v is None:
        return None
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


def make_panel(environ: Any = None) -> Optional[ObsbotPanel]:
    """The panel, or None when it is off (EVE_OBSBOT=0) or OBSBOT Center isn't installed. Making it sends nothing."""
    return ObsbotPanel(environ=environ) if enabled(environ) else None
