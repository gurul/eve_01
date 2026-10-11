"""The OBSBOT webcam's standard USB video (UVC) controls, straight from this Mac: pan, tilt, a continuous turn,
zoom, and waking it. No OBSBOT Center (docs/obsbot.md, "Direct USB").

Owner, 2026-10-11, after OBSBOT Center crashed on an OSC sleep, left an orphaned helper holding its OSC port, showed
"already running" and turned its own OSC setting off: "can't u just interface with [the camera] directly". The
camera's pan/tilt and zoom are UVC camera-terminal controls, so they need no OBSBOT protocol. The Mac's UVC driver
owns the camera and libusb is refused, but IOKit's DeviceRequest reaches the default pipe, and the Tiny 3 answers.
That is ``native/obsbot_uvc.c``, built with clang on first use into BIN_DIR (keyed by the source's hash, so an
edit rebuilds it). One call is one process, ~7 ms.

Measured on a Tiny 3, 2026-10-11:

* Ranges: pan ±130°, tilt ±90°, zoom 0-100. Absolute pan + turns the view left (checked on the picture), tilt + is
  up. Absolute moves land within ~1°, small ones too (a 20° pan took ~2 s).
* The continuous turn's "+1" turns pan the other way (absolute pan goes down): ``turn`` maps directions here so
  callers never see it. At the camera's top speed (80) it turns ~32°/s.
* Asleep, the camera takes commands and ignores them, and reads tilt -79 (parked lens-down). Opening the video
  wakes it: ~1 s of ffmpeg reading AVFoundation, then the gimbal unparks. After the video closed it stayed awake
  for at least a minute.
"""

from __future__ import annotations

import contextlib
import hashlib
import json
import logging
import re
import shutil
import subprocess
import time
from pathlib import Path
from typing import Any, Callable, Optional

log = logging.getLogger(__name__)

SOURCE = Path(__file__).with_name("native") / "obsbot_uvc.c"
BIN_DIR = Path("~/.config/eve-bridge/bin").expanduser()
VENDOR = 0x3564                       # OBSBOT (13668), as native/obsbot_uvc.c
PARKED_TILT = -70.0                   # below this the camera is asleep, lens parked down (it reads -79)
TILT_LIMIT = 59.0                     # moves stay above the parked tilt, so asleep and pointed-down never mix
TURN_SPEED = 40                       # the continuous turn: half the Tiny 3's top speed, ~16°/s, to stop on target
WAKE_SECS = 6.0
NO_CLANG_LINE = "eve needs the Xcode command line tools to reach the camera directly (xcode-select --install)."
NO_CAMERA_LINE = "No OBSBOT camera is plugged into this Mac."

Runner = Callable[..., Any]


def helper_path() -> Path:
    digest = hashlib.sha256(SOURCE.read_bytes()).hexdigest()[:10]
    return BIN_DIR / f"obsbot-uvc-{digest}"


def build(runner: Runner = subprocess.run) -> tuple[Optional[Path], str]:
    """The helper, built if this source has no binary yet. (path, "") or (None, the reason)."""
    path = helper_path()
    if path.exists():
        return path, ""
    clang = shutil.which("clang") or ("/usr/bin/clang" if Path("/usr/bin/clang").exists() else None)
    if not clang:
        return None, NO_CLANG_LINE
    BIN_DIR.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(".tmp")
    out = runner([clang, "-O2", "-framework", "IOKit", "-framework", "CoreFoundation", "-o", str(tmp), str(SOURCE)],
                 capture_output=True, text=True, timeout=120)
    if out.returncode != 0 or not tmp.exists():
        log.warning("obsbot: the USB helper didn't build: %s", (out.stderr or "")[-300:])
        return None, "eve couldn't build its camera helper (clang failed); see the daemon log."
    tmp.replace(path)
    for old in BIN_DIR.glob("obsbot-uvc-*"):         # earlier builds of an older source
        if old != path:
            old.unlink(missing_ok=True)
    log.info("obsbot: built the USB helper %s", path.name)
    return path, ""


def present(runner: Runner = subprocess.run) -> bool:
    """An OBSBOT USB device is plugged in (ioreg, ~50 ms; no helper, no clang)."""
    try:
        out = runner(["ioreg", "-p", "IOUSB", "-l", "-w0"], capture_output=True, text=True, timeout=5,
                     stdin=subprocess.DEVNULL)
    except (OSError, subprocess.TimeoutExpired):
        return False
    return f'"idVendor" = {VENDOR}' in (out.stdout or "")


class UsbCamera:
    """The camera over USB. Every method is quick and blocking (the panel runs them in a thread)."""

    def __init__(self, runner: Runner = subprocess.run, waker: Optional[Callable[[], None]] = None,
                 helper: Optional[Path] = None):
        self._run = runner
        self._waker = waker or _open_video
        self._helper = helper
        self.why = ""

    def _call(self, *args: str) -> dict[str, Any]:
        if self._helper is None:
            self._helper, why = build(self._run)
            if self._helper is None:
                self.why = why
                return {"ok": False, "error": why}
        try:
            out = self._run([str(self._helper), *args], capture_output=True, text=True, stdin=subprocess.DEVNULL,
                            timeout=5)
            reply = json.loads((out.stdout or "").strip().splitlines()[-1])
        except (subprocess.TimeoutExpired, ValueError, IndexError, OSError) as e:
            return {"ok": False, "error": f"the camera helper failed ({type(e).__name__})"}
        if not reply.get("ok"):
            self.why = NO_CAMERA_LINE if reply.get("error") == "no camera" else str(reply.get("error") or "")
        return reply

    def read(self) -> Optional[dict[str, Any]]:
        """pan, tilt (degrees, tilt + up), zoom, their ranges, and asleep; None when the camera can't be read."""
        r = self._call("get")
        if not r.get("ok"):
            return None
        r["asleep"] = float(r["tilt"]) < PARKED_TILT
        return r

    def wake(self) -> bool:
        """Open the video for a moment; True once the gimbal has left its parked tilt."""
        st = self.read()
        if st is None:
            return False
        if not st["asleep"]:
            return True
        self._waker()
        end = time.monotonic() + WAKE_SECS
        while time.monotonic() < end:
            time.sleep(0.3)
            st = self.read()
            if st is not None and not st["asleep"]:
                return True
        return False

    def point(self, pan: float, tilt: float) -> bool:
        return bool(self._call("pantilt", f"{pan:.2f}", f"{tilt:.2f}").get("ok"))

    def turn(self, direction: str, speed: int = TURN_SPEED) -> bool:
        """Start a continuous turn ("left", "right", "up", "down"), or stop with "stop"."""
        pan, tilt = {"left": (-1, 0), "right": (1, 0), "up": (0, 1), "down": (0, -1), "stop": (0, 0)}[direction]
        return bool(self._call("turn", str(pan), str(tilt), str(speed)).get("ok"))

    def zoom(self, level: int) -> bool:
        return bool(self._call("zoom", str(int(level))).get("ok"))


def _open_video() -> None:
    """Read about a second of the camera's video: that wakes a sleeping OBSBOT."""
    ffmpeg = shutil.which("ffmpeg") or next((p for p in ("/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg")
                                             if Path(p).exists()), None)
    if not ffmpeg:
        return
    with contextlib.suppress(Exception):
        subprocess.run([ffmpeg, "-hide_banner", "-loglevel", "error", "-f", "avfoundation", "-pixel_format", "nv12",
                        "-framerate", "30", "-video_size", "1280x720", "-i", _video_device(ffmpeg) + ":none",
                        "-frames:v", "30", "-f", "null", "-"], capture_output=True, stdin=subprocess.DEVNULL,
                       timeout=10)


def _video_device(ffmpeg: str) -> str:
    """The OBSBOT camera's AVFoundation index, by name (the list's order changes); "0" when it isn't named."""
    with contextlib.suppress(Exception):
        err = subprocess.run([ffmpeg, "-hide_banner", "-f", "avfoundation", "-list_devices", "true", "-i", ""],
                             capture_output=True, text=True, stdin=subprocess.DEVNULL, timeout=10).stderr
        for line in err.splitlines():
            if "audio devices" in line:
                break
            m = re.search(r"\[(\d+)\] .*OBSBOT", line)
            if m:
                return m.group(1)
    return "0"
