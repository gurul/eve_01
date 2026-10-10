"""The Mac's own camera: one still, on request.

take_photo was the robot's camera (daemon._photo_for_owner snaps the board). The robot body was retired on
2026-10-03, and from then "take a photo from my Mac and send it to me" had no tool: the brain handed it to
start_task, Codex's Computer Use took the picture in about 70 s, and it reached the chat as a file, not a photo
(owner, 2026-10-06). This is the tool's other camera: ffmpeg reads one frame from AVFoundation in about a second.

The camera is ``EVE_MAC_CAMERA``: an AVFoundation device name or index (``ffmpeg -f avfoundation
-list_devices true -i ""`` lists them), or ``default``, the Mac's default camera, when unset. ``off`` turns the
Mac camera off. The first frames after a webcam opens are dark while its exposure settles, so the still is frame
WARMUP_FRAMES (half a second at 30 fps; measured 2026-10-06 on an OBSBOT Tiny 3: frame 5 had mean luma 134,
frame 15 had 189). It is scaled to at most MAX_WIDTH wide: a 4K webcam's still is more than a chat needs.

macOS asks for camera access once, for the process that runs ffmpeg (the daemon). A refusal, a missing ffmpeg
or a camera in use by another app comes back as a reason, never an exception.
"""

from __future__ import annotations

import asyncio
import logging
import os
import shutil
from typing import Any, Optional

log = logging.getLogger(__name__)

ENV = "EVE_MAC_CAMERA"
DEFAULT_DEVICE = "default"
WARMUP_FRAMES = 15
FRAMERATE = 30
MAX_WIDTH = 1920
TIMEOUT_SECS = 12.0
FFMPEG_PATHS = ("/opt/homebrew/bin/ffmpeg", "/usr/local/bin/ffmpeg")   # launchd's PATH has neither


def device(environ: Any = None) -> Optional[str]:
    """The AVFoundation device to read, or None when the Mac camera is off."""
    env = os.environ if environ is None else environ
    raw = str(env.get(ENV, "") or "").strip()
    if raw.lower() in ("off", "0", "false", "no", "none"):
        return None
    return raw or DEFAULT_DEVICE


def ffmpeg_path() -> Optional[str]:
    found = shutil.which("ffmpeg")
    if found:
        return found
    return next((p for p in FFMPEG_PATHS if os.access(p, os.X_OK)), None)


def command(ffmpeg: str, dev: str) -> list[str]:
    """One JPEG of frame WARMUP_FRAMES on stdout. ``<dev>:none`` opens the camera without a microphone."""
    return [ffmpeg, "-hide_banner", "-loglevel", "error", "-nostdin",
            "-f", "avfoundation", "-pixel_format", "nv12", "-framerate", str(FRAMERATE), "-i", f"{dev}:none",
            "-vf", f"select=gte(n\\,{WARMUP_FRAMES}),scale='min({MAX_WIDTH},iw)':-2",
            "-frames:v", "1", "-q:v", "3", "-f", "image2pipe", "-vcodec", "mjpeg", "pipe:1"]


async def snap(environ: Any = None, timeout: float = TIMEOUT_SECS) -> dict[str, Any]:
    """{"ok": True, "jpeg": bytes} or {"ok": False, "reason": ...}."""
    dev = device(environ)
    if dev is None:
        return {"ok": False, "reason": "the Mac's camera is switched off for eve"}
    ffmpeg = ffmpeg_path()
    if ffmpeg is None:
        return {"ok": False, "reason": "ffmpeg is not installed, so eve cannot read the Mac's camera"}
    try:
        proc = await asyncio.create_subprocess_exec(*command(ffmpeg, dev), stdin=asyncio.subprocess.DEVNULL,
                                                    stdout=asyncio.subprocess.PIPE,
                                                    stderr=asyncio.subprocess.PIPE)
    except OSError as e:
        log.warning("mac camera: cannot start ffmpeg: %s", e)
        return {"ok": False, "reason": "the Mac's camera could not be started"}
    try:
        out, err = await asyncio.wait_for(proc.communicate(), timeout=timeout)
    except asyncio.TimeoutError:
        proc.kill()
        await proc.wait()
        return {"ok": False, "reason": "the Mac's camera did not answer in time"}
    if proc.returncode != 0 or out[:2] != b"\xff\xd8":
        tail = (err or b"").decode("utf-8", "replace").strip().splitlines()[-1:] or [""]
        log.warning("mac camera: no picture (exit %s): %s", proc.returncode, tail[0][:200])
        return {"ok": False, "reason": "the Mac's camera gave no picture (is another app using it, "
                                       "or is camera access off for eve?)"}
    return {"ok": True, "jpeg": out}
