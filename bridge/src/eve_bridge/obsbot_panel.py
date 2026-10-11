"""The OBSBOT camera in Eve Link's Apps tab (docs/obsbot.md; owner, 2026-10-11: "build in an app into eve for
obsbot"): the camera's live picture, hold-to-turn arrows, zoom, center, sleep and wake, tracking and the AI mode.

The phone never talks to the camera. Every press comes through the tunnel to the daemon with the link token
(link_chat.py, ``POST /api/link/controls/obsbot``), and this panel drives OBSBOT Center's OSC server (obsbot.py).

Holding an arrow is Pika's hold-to-drive (pika.py): the app sends ``hold`` with the press's id about every 150 ms,
and every beat sends the turn again (OSC ``SetGimbalUp`` etc. at a speed, which turns until a 0 is sent). Not only
the first: OSC is UDP, so a lost first turn would otherwise leave a held arrow doing nothing. The gimbal stops on ``stop``, when no beat came for HEARTBEAT_SECS, on ``close`` and when the
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
import logging
import os
import re
import shutil
import time
from typing import Any, AsyncIterator, Callable, Optional

from . import obsbot, pika_rtc
from .pika import DEFAULT_STUN as STUN
from .pika import VIDEO_HEAD, _chunk, _json, split_jpegs, stamp_ms, video_part

log = logging.getLogger(__name__)

HEARTBEAT_SECS = 0.45                 # no beat from the app for this long: stop turning
HOLD_SPEED = 60                       # SetGimbalUp etc., 1-100: ~25°/s, slow enough to stop where you meant
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


def enabled(environ: Any = None) -> bool:
    env = os.environ if environ is None else environ
    return env.get("EVE_OBSBOT", "1").strip() != "0" and obsbot.APP_PATH.exists()


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
        self._tx: Optional[obsbot.Obsbot] = None       # beats and stops: sends only, never queued behind a read
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
        """None when OBSBOT Center answers (it is started hidden when it isn't running); else the owner's line."""
        if self._cam is not None and self._cam._ready:
            return None                                 # no lock: a beat must not wait behind a move's read-back
        async with self._lock:
            if self._cam is None:
                self._cam = self._make()
                self._tx = self._make(launch=False)
            why = await asyncio.to_thread(self._cam.ensure)
            if why is None and self._tx is not None:
                self._tx._ready = True                  # the same app: _cam just proved it answers
            return why

    async def _read(self, fn: Callable[..., dict[str, Any]], *args: Any) -> dict[str, Any]:
        async with self._lock:
            res = await asyncio.to_thread(fn, *args)
        if not res.get("ok") and self._cam is not None:
            self._cam._ready = False                    # ask again next time: OBSBOT Center may have quit
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

    # -- hold to turn --
    async def _turn(self, hold: str, direction: str) -> dict[str, Any]:
        if not hold or direction not in obsbot.DIRECTIONS:
            return {"ok": False, "line": "Hold an arrow.", "stopped": True}
        if hold in self._ended:
            return {"ok": False, "line": "", "stopped": True}  # a late beat of a press that ended
        tx = self._tx
        assert tx is not None
        if self._hold != (hold, direction):
            if self._hold is not None and self._hold[1] != direction:
                tx.send(obsbot.G + obsbot.DIRECTIONS[self._hold[1]], tx.device, 0)
            if self._hold is not None:
                self._end(self._hold[0])
            self._hold = (hold, direction)
        tx.send(obsbot.G + obsbot.DIRECTIONS[direction], tx.device, HOLD_SPEED)   # every beat: UDP (see the top)
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
        if self._tx is not None:
            for cmd in obsbot.DIRECTIONS.values():
                self._tx.send(obsbot.G + cmd, self._tx.device, 0)

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
            stdout=asyncio.subprocess.DEVNULL, stderr=asyncio.subprocess.PIPE)
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
            stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
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
        for c in (self._cam, self._tx):
            if c is not None:
                c.close()


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
