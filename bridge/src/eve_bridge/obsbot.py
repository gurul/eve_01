"""The owner's OBSBOT Tiny 3 webcam: point the gimbal, zoom, wake and sleep, presets and AI tracking — through
OBSBOT Center's OSC server on this Mac.

The owner asked on 2026-10-11 for a CLI so eve can control the camera. How OBSBOT Center talks to the camera was
read out of the app (v2.0.15) with REA (github.com/morluto/rea) and ``strings``/``nm``: the camera itself is driven
by ``Frameworks/libdev.dylib`` (OBSBOT's device SDK, C++ ``Device::ai*``/``camera*`` methods over USB), and the app
wraps a liblo ``OSCServer`` around it with 76 addresses under ``/OBSBOT/...``. OBSBOT documents that OSC surface
(obsbot.com/explore/obsbot-center/osc, ``obsbot_center_osc_definition20260508.xlsx``), so eve speaks OSC rather than
re-implementing the USB protocol: it keeps working across firmware updates, and the app stays usable by hand.
The cost is that OBSBOT Center must be running with OSC on; eve starts it when it is not
and hides its window, and ``eve-bridge obsbot setup`` turns OSC on in its ``global.ini``.

The wire, measured against a Tiny 3 on 2026-10-11:

* UDP to 127.0.0.1:16284 (``OSCReceivePort``). Replies come back to the sender's address and port, from 16284.
  OBSBOT Center binds ``*:16284``, so anything on the LAN can send it commands too; that is the app's choice.
* Every command's first int is the device index (0 = the first camera). It is optional in OBSBOT's sheet; eve
  always sends it. A query (``Get...``) takes the index **alone**: the sheet's reserved second int makes
  ``GetDeviceInfo`` go unanswered.
* ``/General/Connected`` is answered with ``ConnectedResp 1`` and then a push of DeviceInfo, ZoomInfo, the gimbal
  position, the presets and the AI lock state. That is how eve asks "is the app there" (~5 ms).
* ``GetGimbalPosInfoResp`` carries **two** ints, ``[pan, pitch]`` in degrees, not the three (roll, pitch, yaw)
  the sheet says. Measured by commanding (40, 0), (0, 25) and reading them back. Positive pan turns the view
  left (checked on the picture: the scene's left edge came into frame); positive pitch tilts it **down** (holding
  "up" reads negative, and asleep the lens parks at +79).
* ``ZoomInfo`` is ``[zoom 0-100, fov]``; the Tiny 3 answers fov -1, so the field of view is set but never read.
* ``DeviceInfo`` is ``[connected, name] x 4, selected, run state (0 sleep, 1 awake), type]``; a Tiny 3 is type 18.
* ``AiTrackingInfo`` is the target *lock*, not whether tracking is on: it stayed 0 after SetAiMode with no one
  in the frame. So a tracking change is sent and not claimed as read back.
* Commands whose value the sheet marks "x" (``ResetGimbal``, like the queries) act only when sent the device
  index alone; with the extra 0 the reset was ignored. Even alone it was ignored one time in three, so "center"
  is a move to (0, 0).
* ``SetGimMotorDegree`` (ints) is the move. Its float twin ``SetGimMotorDegreeEx`` moved pan but left pitch where
  it was. The speed argument decides whether a move lands: at 60 every move under ~20° on an axis was dropped,
  at 90 small moves overshot, at 40 two of six missed; at 30 all landed within 2°, at about 25°/s (a 180° pan in
  7.2 s). So eve sends 30, waits for the distance, and resends a move that stopped short once at 20.
* ``SetZoomSpeed`` did nothing on the Tiny 3 (any argument shape); ``SetZoom`` works, ~2 s end to end.
* ``WakeSleep 1`` sent while the camera is still parking from a sleep is ignored; from rest it wakes in ~0.6 s.
  Asleep, the gimbal reads pitch 79 (parked lens-down).

The commands that move the camera, ``move`` and ``zoom``, are read back until the camera arrives and the reply
says where it ended up. The Tiny 3's AI modes are its own numbering (single, group, voice, desk, hand,
whiteboard), which differs from the Tiny 2's; ``mode`` refuses a camera that is not a Tiny 3 rather than guess.
"""

from __future__ import annotations

import contextlib
import json
import socket
import struct
import subprocess
import time
from pathlib import Path
from typing import Any, Callable, Optional, Sequence

from . import obsbot_usb
from .obsbot_usb import UsbCamera

HOST = "127.0.0.1"
PORT = 16284
APP = "OBSBOT_Center"
APP_PATH = Path("/Applications/OBSBOT_Center.app")
INI_PATH = Path("~/Library/Application Support/OBSBOT_Center/global.ini")
ARRIVE_WAIT = 4.0           # zoom: 0 to 100% takes ~2 s

G = "/OBSBOT/WebCam/General/"
T = "/OBSBOT/WebCam/Tiny/"

PAN_RANGE = (-129.0, 129.0)
PITCH_RANGE = (-obsbot_usb.TILT_LIMIT, obsbot_usb.TILT_LIMIT)     # above the parked tilt: asleep stays telling
DIRECTIONS = {"up": "SetGimbalUp", "down": "SetGimbalDown", "left": "SetGimbalLeft", "right": "SetGimbalRight"}
FOV = {86: 0, 78: 1, 65: 2}
# Tiny 3 / Tiny 3 Lite numbering for SetAiMode (OBSBOT's sheet, 2026-05-08)
AI_MODES = {"single": 0, "group": 1, "voice": 2, "desk": 3, "hand": 4, "whiteboard": 5}
FRAMING = {"headroom": 0, "standard": 1, "motion": 2}
TRACK_SPEED = {"slow": 0, "standard": 1, "fast": 2}

OFF_REASON = "OBSBOT Center is running but isn't answering on OSC. Run: eve-bridge obsbot setup"
APP_NEEDED_LINE = "This needs OBSBOT Center running (tracking, AI modes, presets and sleep are OBSBOT's own; eve doesn't start it)."


# --- OSC 1.0, the subset OBSBOT uses: int32, float32, string ---------------------------------------------------

def _pad(b: bytes) -> bytes:
    return b + b"\0" * (4 - len(b) % 4)


def encode(address: str, *args: Any) -> bytes:
    tags, body = ",", b""
    for a in args:
        if isinstance(a, bool) or isinstance(a, int):
            tags, body = tags + "i", body + struct.pack(">i", int(a))
        elif isinstance(a, float):
            tags, body = tags + "f", body + struct.pack(">f", a)
        else:
            tags, body = tags + "s", body + _pad(str(a).encode())
    return _pad(address.encode()) + _pad(tags.encode()) + body


def decode(data: bytes) -> tuple[str, list[Any]]:
    def string(i: int) -> tuple[str, int]:
        j = data.index(b"\0", i)
        return data[i:j].decode("utf-8", "replace"), (j // 4 + 1) * 4

    address, i = string(0)
    tags, i = string(i) if i < len(data) else (",", i)
    values: list[Any] = []
    for t in tags[1:]:
        if t == "i":
            values.append(struct.unpack(">i", data[i:i + 4])[0])
            i += 4
        elif t == "f":
            values.append(round(struct.unpack(">f", data[i:i + 4])[0], 2))
            i += 4
        elif t == "s":
            s, i = string(i)
            values.append(s)
        else:
            break                                   # a type OBSBOT never sends: stop rather than misread
    return address, values


# --- the camera -------------------------------------------------------------------------------------------------

def _app_running() -> bool:
    return subprocess.run(["pgrep", "-x", "OBSBOT_Main"], capture_output=True).returncode == 0


class Obsbot:
    """The camera: moves, turns, zoom, status and waking over USB (obsbot_usb.py), always; tracking, the AI mode,
    presets, sleep and the picture settings over OBSBOT Center's OSC, only while the app is running. eve never
    starts the app (owner, 2026-10-11: direct, with the app for the AI features)."""

    def __init__(self, host: str = HOST, port: int = PORT, device: int = 0,
                 app_running: Callable[[], bool] = _app_running, usb: Optional[UsbCamera] = None):
        self.addr = (host, port)
        self.device = device
        self._app_running = app_running
        self.usb = usb or UsbCamera()
        self.sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        self.sock.bind(("127.0.0.1" if host in ("127.0.0.1", "localhost") else "", 0))
        self._ready = False                         # the camera answered over USB
        self._osc_ready = False                     # OBSBOT Center answered over OSC

    def close(self) -> None:
        """Tell OBSBOT Center this client is gone (its sheet's optional ``Disconnected``), then close. Every source
        port that ever sent is a client it keeps and refreshes on each state change."""
        if self._osc_ready:
            with contextlib.suppress(OSError):
                self.send(G + "Disconnected", 0)
        self.sock.close()

    # the OSC wire (OBSBOT Center)
    def send(self, name: str, *args: Any) -> None:
        address = name if name.startswith("/") else G + name
        self.sock.sendto(encode(address, *args), self.addr)

    def _recv_until(self, want: str, timeout: float) -> Optional[list[Any]]:
        end = time.monotonic() + timeout
        while True:
            left = end - time.monotonic()
            if left <= 0:
                return None
            self.sock.settimeout(left)
            try:
                data, _ = self.sock.recvfrom(65536)
            except (socket.timeout, OSError):
                return None
            try:
                address, values = decode(data)
            except (ValueError, struct.error, IndexError):
                continue
            if address.endswith(want):
                return values

    def _recv_raw(self, timeout: float) -> Optional[tuple[str, list[Any]]]:
        self.sock.settimeout(timeout)
        try:
            return decode(self.sock.recvfrom(65536)[0])
        except (socket.timeout, OSError, ValueError, struct.error, IndexError):
            return None

    def _drain(self) -> None:
        self.sock.settimeout(0)
        try:
            while True:
                self.sock.recvfrom(65536)
        except (BlockingIOError, OSError):
            pass

    def ask(self, name: str, reply: str, timeout: float = 1.0) -> Optional[list[Any]]:
        self._drain()
        self.send(name, self.device)                # one int: GetDeviceInfo with a second (the sheet's "x") never answers
        return self._recv_until(reply, timeout)

    def ensure(self) -> Optional[str]:
        """None when the camera answers over USB; otherwise the reason it can't (no camera, no clang)."""
        if self._ready:
            return None
        if self.usb.read() is None:
            return self.usb.why or obsbot_usb.NO_CAMERA_LINE
        self._ready = True
        return None

    def osc(self) -> Optional[str]:
        """None when OBSBOT Center answers on OSC; otherwise the line for a feature that needs it. Never starts it."""
        if self._osc_ready:
            return None
        self._drain()
        self.send(G + "Connected", 0)
        if self._recv_until("ConnectedResp", 0.5) is not None:
            self._osc_ready = True
            return None
        if self._app_running():
            return OFF_REASON
        return APP_NEEDED_LINE

    # reads
    def info(self) -> Optional[dict[str, Any]]:
        """OBSBOT Center's device list (OSC); None when the app isn't there."""
        if self.osc():
            return None
        v = self.ask(G + "GetDeviceInfo", "DeviceInfo")
        if not v or len(v) < 11:
            return None
        devices = [{"index": n, "connected": bool(v[2 * n]), "name": v[2 * n + 1]}
                   for n in range(4) if v[2 * n] or v[2 * n + 1]]
        return {"devices": devices, "selected": v[8], "awake": bool(v[9]), "type": v[10]}

    def position(self) -> Optional[tuple[int, int]]:
        """(pan, pitch) in degrees: pan + left, pitch + down (the camera's tilt + up, turned over)."""
        st = self.usb.read()
        return (round(st["pan"]), -round(st["tilt"])) if st else None

    def zoom_level(self) -> Optional[int]:
        st = self.usb.read()
        return int(st["zoom"]) if st and st.get("zoom") is not None else None

    def presets(self) -> list[dict[str, Any]]:
        if self.osc():
            return []
        v = self.ask(T + "GetPresetPositionInfo", "PresetPositionInfo") or []
        return [{"n": n + 1, "set": bool(v[2 * n]), "name": v[2 * n + 1] if v[2 * n] else ""}
                for n in range(len(v) // 2)]

    def position_reply(self) -> dict[str, Any]:
        pos = self.position()
        if not pos:
            return {"ok": False, "line": self.usb.why or "Couldn't read where the camera points."}
        return {"ok": True, "pan": pos[0], "pitch": pos[1], "line": ""}

    def zoom_reply(self) -> dict[str, Any]:
        z = self.zoom_level()
        return {"ok": True, "zoom": z, "line": ""} if z is not None else {"ok": False, "line": "Couldn't read the zoom."}

    def locked(self) -> Optional[bool]:
        if self.osc():
            return None
        v = self.ask(T + "GetAiTrackingInfo", "AiTrackingInfo")
        return bool(v[0]) if v else None

    # the doors
    def status(self) -> dict[str, Any]:
        st = self.usb.read()
        if st is None:
            return {"ok": False, "line": self.usb.why or obsbot_usb.NO_CAMERA_LINE}
        name = str(st.get("name") or "OBSBOT camera").removeprefix("OBSBOT ").strip() or "OBSBOT camera"
        awake = not st["asleep"]
        pan, pitch = round(st["pan"]), -round(st["tilt"])
        zoom = st.get("zoom")
        presets, lock = self.presets(), self.locked()
        parts = [f"{name} {'awake' if awake else 'asleep'}"]
        if awake:
            parts.append(f"pan {pan}°, pitch {pitch}°")
        if zoom is not None:
            parts.append(f"zoom {zoom}%")
        if lock:
            parts.append("tracking a locked target")
        saved = [p["name"] or f"preset {p['n']}" for p in presets if p["set"]]
        if saved:
            parts.append("presets: " + ", ".join(saved))
        return {"ok": True, "line": "; ".join(parts) + ".", "camera": name, "awake": awake,
                "pan": pan if awake else None, "pitch": pitch if awake else None, "zoom": zoom,
                "locked": lock, "presets": presets, "app": self._osc_ready}

    def wake(self, awake: bool) -> dict[str, Any]:
        name = (self.usb.read() or {}).get("name", "The camera").removeprefix("OBSBOT ")
        if awake:
            if self.usb.wake():
                return {"ok": True, "line": f"{name} awake."}
            return {"ok": False, "line": self.usb.why or "The camera didn't wake. Is eve allowed to use the camera?"}
        if why := self.osc():                       # sleep is OBSBOT's own command: no UVC control for it
            return {"ok": False, "line": why}
        for _ in range(2):                          # a sleep sent mid-unpark is ignored: send it again once
            self.send(G + "WakeSleep", self.device, 0)
            end = time.monotonic() + 5.0
            while time.monotonic() < end:
                time.sleep(0.5)
                st = self.usb.read()
                if st and st["asleep"]:
                    return {"ok": True, "line": f"{name} asleep."}
                if not self._app_running():
                    self._osc_ready = False         # its sleep handler can crash the app (docs/obsbot.md)
                    return {"ok": False, "line": "OBSBOT Center crashed putting the camera to sleep.", "crashed": True}
        return {"ok": False, "line": "Sent sleep; the camera didn't confirm it."}

    def move(self, pan: Optional[float], pitch: Optional[float]) -> dict[str, Any]:
        if not self.usb.wake():
            return {"ok": False, "line": self.usb.why or "The camera is asleep and didn't wake."}
        now = self.position()
        if not now:
            return {"ok": False, "line": "Couldn't read where the camera points."}
        pan = round(max(PAN_RANGE[0], min(PAN_RANGE[1], now[0] if pan is None else float(pan))))
        pitch = round(max(PITCH_RANGE[0], min(PITCH_RANGE[1], now[1] if pitch is None else float(pitch))))
        if not self.usb.point(pan, -pitch):
            return {"ok": False, "line": "The camera refused the move."}
        far = max(abs(pan - now[0]), abs(pitch - now[1]))
        return self._settle(pan, pitch, wait=min(12.0, 1.0 + far / 15))

    def _settle(self, pan: float, pitch: float, wait: float = ARRIVE_WAIT) -> dict[str, Any]:
        start = time.monotonic()
        last, pos, still = None, None, 0
        while time.monotonic() < start + wait + 1.5:
            time.sleep(0.25)
            pos = self.position()
            still = still + 1 if pos and pos == last else 0
            last = pos
            # still for three reads (~0.75 s): arrived, or stopped short (an end stop). Within a few degrees is
            # not enough: the last degree creeps in, and the reply should say where the camera ended up
            if still >= 3 or (still >= 1 and time.monotonic() > start + wait):
                break
        if not pos:
            return {"ok": False, "line": "Sent the move; couldn't read the camera's position back."}
        arrived = abs(pos[0] - pan) <= 3 and abs(pos[1] - pitch) <= 3
        line = f"Pan {pos[0]}°, pitch {pos[1]}°."
        if not arrived:
            line = f"Asked for pan {pan:g}°, pitch {pitch:g}°; it stopped at pan {pos[0]}°, pitch {pos[1]}°."
        return {"ok": arrived, "line": line, "pan": pos[0], "pitch": pos[1]}

    def nudge(self, direction: str, speed: int = obsbot_usb.TURN_SPEED, seconds: float = 0.5) -> dict[str, Any]:
        if direction not in DIRECTIONS:
            return {"ok": False, "line": f"Direction is one of: {', '.join(DIRECTIONS)}."}
        if not self.usb.wake():
            return {"ok": False, "line": self.usb.why or "The camera is asleep and didn't wake."}
        self.usb.turn(direction, max(1, min(127, int(speed))))
        time.sleep(max(0.05, min(10.0, seconds)))
        self.usb.turn("stop")
        time.sleep(0.4)
        pos = self.position()
        tail = f" Pan {pos[0]}°, pitch {pos[1]}°." if pos else ""
        return {"ok": True, "line": f"Moved {direction}.{tail}", "pan": pos[0] if pos else None,
                "pitch": pos[1] if pos else None}

    def stop(self) -> dict[str, Any]:
        self.usb.turn("stop")
        return {"ok": True, "line": "Stopped."}

    def reset(self) -> dict[str, Any]:
        """Back to straight ahead: a move to (0, 0)."""
        res = self.move(0, 0)
        return {**res, "line": "Centered." if res["ok"] else res["line"]}

    def zoom(self, level: int) -> dict[str, Any]:
        level = max(0, min(100, int(level)))
        if not self.usb.wake():
            return {"ok": False, "line": self.usb.why or "The camera is asleep and didn't wake."}
        if not self.usb.zoom(level):
            return {"ok": False, "line": "The camera refused the zoom."}
        end, got = time.monotonic() + ARRIVE_WAIT, None
        while time.monotonic() < end:
            time.sleep(0.3)
            got = self.zoom_level()
            if got is not None and abs(got - level) <= 1:
                return {"ok": True, "line": f"Zoom {got}%.", "zoom": got}
        return {"ok": False, "line": f"Asked for zoom {level}%; it reads {got}%.", "zoom": got}

    # OBSBOT's own features: OBSBOT Center, while it runs
    def _app(self) -> Optional[dict[str, Any]]:
        why = self.osc()
        return {"ok": False, "line": why} if why else None

    def fov(self, degrees: int) -> dict[str, Any]:
        if degrees not in FOV:
            return {"ok": False, "line": "Field of view is 86, 78 or 65."}
        if refused := self._app():
            return refused
        self.send(G + "SetView", self.device, FOV[degrees])
        return {"ok": True, "line": f"Field of view {degrees}°."}

    def preset(self, n: int) -> dict[str, Any]:
        if n not in (1, 2, 3):
            return {"ok": False, "line": "Preset is 1, 2 or 3."}
        if refused := self._app():
            return refused
        saved = {p["n"]: p for p in self.presets()}
        if saved and not saved.get(n, {}).get("set"):
            return {"ok": False, "line": f"Preset {n} isn't saved. Save it in OBSBOT Center first."}
        self.send(T + "TriggerPreset", self.device, n - 1)
        name = saved.get(n, {}).get("name") or f"preset {n}"
        return {"ok": True, "line": f"Going to {name}."}

    def track(self, on: bool) -> dict[str, Any]:
        if refused := self._app():
            return refused
        self.send(T + "ToggleAILock", self.device, 1 if on else 0)
        return {"ok": True, "line": "Tracking on." if on else "Tracking off."}

    def _tiny3(self) -> Optional[str]:
        name = str((self.usb.read() or {}).get("name") or "")
        if name and "tiny 3" not in name.lower():
            return f"These settings use the Tiny 3's numbering; this camera is a {name}."
        return None

    def mode(self, name: str) -> dict[str, Any]:
        if name not in AI_MODES:
            return {"ok": False, "line": f"AI mode is one of: {', '.join(AI_MODES)}."}
        if why := self._tiny3():
            return {"ok": False, "line": why}
        if refused := self._app():
            return refused
        self.send(T + "SetAiMode", self.device, AI_MODES[name])
        return {"ok": True, "line": f"AI mode {name}."}

    def framing(self, name: str) -> dict[str, Any]:
        if name not in FRAMING:
            return {"ok": False, "line": f"Framing is one of: {', '.join(FRAMING)}."}
        if refused := self._app():
            return refused
        self.send(T + "SetTrackingMode", self.device, FRAMING[name])
        return {"ok": True, "line": f"Framing {name}."}

    def track_speed(self, name: str) -> dict[str, Any]:
        if name not in TRACK_SPEED:
            return {"ok": False, "line": f"Tracking speed is one of: {', '.join(TRACK_SPEED)}."}
        if why := self._tiny3():
            return {"ok": False, "line": why}
        if refused := self._app():
            return refused
        self.send(T + "SetTrackingSpeed", self.device, TRACK_SPEED[name])
        return {"ok": True, "line": f"Tracking speed {name}."}

    def focus(self, value: str) -> dict[str, Any]:
        if value != "auto":
            try:
                int(value)
            except ValueError:
                return {"ok": False, "line": "Focus is auto or 0-100."}
        if refused := self._app():
            return refused
        if value == "auto":
            self.send(G + "SetAutoFocus", self.device, 1)
            return {"ok": True, "line": "Autofocus on."}
        level = max(0, min(100, int(value)))
        self.send(G + "SetAutoFocus", self.device, 0)
        self.send(G + "SetManualFocus", self.device, level)
        return {"ok": True, "line": f"Manual focus {level}."}

    def mirror(self, on: bool) -> dict[str, Any]:
        if refused := self._app():
            return refused
        self.send(G + "SetMirror", self.device, 1 if on else 0)
        return {"ok": True, "line": "Mirrored." if on else "Not mirrored."}


# --- setup: the USB helper, and OSC for OBSBOT's own features ---------------------------------------------------

def setup(ini: Path = INI_PATH, out: Callable[[str], Any] = print, usb: Optional[UsbCamera] = None) -> int:
    """Build the USB helper and read the camera (all eve needs to move it). Then, when OBSBOT Center is installed
    and not running, turn its OSC on (UDP, 127.0.0.1, port 16284) in global.ini, for tracking, AI modes, presets and
    sleep. A running app is left alone: it rewrites the file when it quits, and asking it to quit shows a dialog."""
    usb = usb or UsbCamera()
    st = usb.read()
    if st is None:
        out(usb.why or obsbot_usb.NO_CAMERA_LINE)
        return 1
    out(f"{st.get('name', 'The camera')} answers over USB: eve can move, zoom and wake it without OBSBOT Center.")
    ini = ini.expanduser()
    if not APP_PATH.exists() or not ini.exists():
        out("OBSBOT Center isn't set up, so tracking, AI modes, presets and sleep are off. That's fine for moving it.")
        return 0
    if _app_running():
        out("OBSBOT Center is running: for tracking and AI modes from eve, turn on OSC in its settings (UDP, port "
            f"{PORT}), or quit it and run this again.")
        return 0
    want = {"OSC": "true", "OSCConnectMethod": "0", "OSCHostIp": HOST, "OSCReceivePort": str(PORT)}
    lines, seen = ini.read_text().splitlines(), set()
    for i, line in enumerate(lines):
        key = line.split("=", 1)[0]
        if key in want and "=" in line:
            lines[i] = f"{key}={want[key]}"
            seen.add(key)
    missing = [k for k in want if k not in seen]
    if missing:
        at = next((i + 1 for i, ln in enumerate(lines) if ln.strip() == "[SoftSetting]"), None)
        if at is None:
            lines += ["[SoftSetting]"]
            at = len(lines)
        lines[at:at] = [f"{k}={want[k]}" for k in missing]
    ini.write_text("\n".join(lines) + "\n")
    out("OSC is on in OBSBOT Center's settings: open the app when you want tracking or AI modes from eve.")
    return 0


# --- the CLI ------------------------------------------------------------------------------------------------------

def _number(s: str) -> Any:
    for kind in (int, float):
        try:
            return kind(s)
        except ValueError:
            pass
    return s


def cli(argv: Sequence[str], out: Callable[[str], Any] = print, make: Callable[..., Obsbot] = Obsbot) -> int:
    """``eve-bridge obsbot setup | status | wake | sleep | move [--pan P] [--pitch P] [--speed S] | nudge DIR
    | stop | center | zoom N [--speed S] | fov 86|78|65 | preset N | track on|off | mode NAME | framing NAME
    | track-speed NAME | focus auto|N | mirror on|off | raw ADDRESS [ARGS...]``. ``--json`` prints the result."""
    import argparse

    p = argparse.ArgumentParser(prog="eve-bridge obsbot", description="Control the OBSBOT camera: moves over USB, "
                                "OBSBOT's own features through OBSBOT Center while it runs.")
    p.add_argument("--json", action="store_true", help="print the result as JSON")
    p.add_argument("--device", type=int, default=0, help="camera index in OBSBOT Center (default 0)")
    sub = p.add_subparsers(dest="act", required=True)
    sub.add_parser("setup", help="Build the USB helper, read the camera, and turn on OBSBOT Center's OSC if it's closed")
    sub.add_parser("status", help="Camera, awake or asleep, where it points, zoom, presets")
    sub.add_parser("wake", help="Wake the camera")
    sub.add_parser("sleep", help="Put the camera to sleep (needs OBSBOT Center running)")
    m = sub.add_parser("move", help="Point the gimbal: pan -129..129° (+ is left), pitch -59..59° (+ is down)")
    m.add_argument("--pan", type=float)
    m.add_argument("--pitch", type=float)
    n = sub.add_parser("nudge", help="Turn the gimbal for a moment: up, down, left, right")
    n.add_argument("direction", choices=list(DIRECTIONS))
    n.add_argument("--speed", type=int, default=obsbot_usb.TURN_SPEED,
                   help=f"1-80 on a Tiny 3 (default {obsbot_usb.TURN_SPEED})")
    n.add_argument("--seconds", type=float, default=0.5)
    sub.add_parser("stop", help="Stop a gimbal move")
    sub.add_parser("center", help="Reset the gimbal to its home position")
    z = sub.add_parser("zoom", help="Zoom 0-100%%")
    z.add_argument("level", type=int)
    f = sub.add_parser("fov", help="Field of view: 86, 78 or 65°")
    f.add_argument("degrees", type=int)
    pr = sub.add_parser("preset", help="Go to a preset saved in OBSBOT Center: 1, 2 or 3")
    pr.add_argument("n", type=int)
    t = sub.add_parser("track", help="AI tracking lock on or off")
    t.add_argument("state", choices=["on", "off"])
    md = sub.add_parser("mode", help="AI mode: " + ", ".join(AI_MODES))
    md.add_argument("name", choices=list(AI_MODES))
    fr = sub.add_parser("framing", help="Tracking framing: " + ", ".join(FRAMING))
    fr.add_argument("name", choices=list(FRAMING))
    ts = sub.add_parser("track-speed", help="Tracking speed: " + ", ".join(TRACK_SPEED))
    ts.add_argument("name", choices=list(TRACK_SPEED))
    fc = sub.add_parser("focus", help="auto, or manual 0-100")
    fc.add_argument("value")
    mi = sub.add_parser("mirror", help="Mirror the picture: on or off")
    mi.add_argument("state", choices=["on", "off"])
    r = sub.add_parser("raw", help="Send any OBSBOT OSC address; ints, floats and strings as given")
    r.add_argument("address")
    r.add_argument("args", nargs="*")
    # the global options are accepted after the subcommand too: "obsbot status --json"
    argv, front = list(argv), []
    for flag in ("--json",):
        while flag in argv:
            argv.remove(flag)
            front.append(flag)
    if "--device" in argv[1:] and argv.index("--device", 1) + 1 < len(argv):
        i = argv.index("--device", 1)
        front += argv[i:i + 2]
        del argv[i:i + 2]
    a = p.parse_args(front + argv)

    if a.act == "setup":
        return setup(out=out)

    cam = make(device=a.device)
    try:
        why = cam.ensure()
        if why:
            res: dict[str, Any] = {"ok": False, "line": why}
        elif a.act == "status":
            res = cam.status()
        elif a.act in ("wake", "sleep"):
            res = cam.wake(a.act == "wake")
        elif a.act == "move":
            res = cam.move(a.pan, a.pitch)
        elif a.act == "nudge":
            res = cam.nudge(a.direction, a.speed, a.seconds)
        elif a.act == "stop":
            res = cam.stop()
        elif a.act == "center":
            res = cam.reset()
        elif a.act == "zoom":
            res = cam.zoom(a.level)
        elif a.act == "fov":
            res = cam.fov(a.degrees)
        elif a.act == "preset":
            res = cam.preset(a.n)
        elif a.act == "track":
            res = cam.track(a.state == "on")
        elif a.act == "mode":
            res = cam.mode(a.name)
        elif a.act == "framing":
            res = cam.framing(a.name)
        elif a.act == "track-speed":
            res = cam.track_speed(a.name)
        elif a.act == "focus":
            res = cam.focus(a.value)
        elif a.act == "mirror":
            res = cam.mirror(a.state == "on")
        else:                                       # raw
            if not a.address.startswith("/OBSBOT/"):
                res = {"ok": False, "line": "The address starts with /OBSBOT/."}
            elif why := cam.osc():
                res = {"ok": False, "line": why}
            else:
                cam._drain()
                cam.send(a.address, *[_number(x) for x in a.args])
                # everything that arrives in the next 0.7 s: the camera also pushes changes unasked
                replies, end = [], time.monotonic() + 0.7
                while (got := cam._recv_raw(max(0.01, end - time.monotonic()))) and time.monotonic() < end + 0.1:
                    replies.append([got[0], got[1]])
                lines = "".join(f"\n  {r[0]} {r[1]}" for r in replies)
                res = {"ok": True, "line": f"Sent {a.address}." + (lines or " No reply."), "replies": replies}
    finally:
        cam.close()
    out(json.dumps(res, ensure_ascii=False) if a.json else res["line"])
    return 0 if res.get("ok") else 1
