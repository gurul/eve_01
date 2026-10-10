"""Pika, the owner's ELEGOO robot car, driven from the Eve Link app through this Mac (docs/pika.md).

Owner, 2026-10-10: "can we build a controller for pika as an app", "Inside Eve Link, via the Mac", "very
minimalist". The app's Pika screen is live video with a hold-to-drive pad over it. It reaches the car only through
this daemon: ``POST /api/link/controls/pika`` with one JSON body (link_chat.LinkChat._controls), and the video at
``GET /api/link/controls/pika/stream``, both behind the link token.

The car (github.com/gurul/pika). An ESP32-S3 camera board on the home Wi-Fi relays JSON frames between one TCP
client on port 100 and the UNO, and serves MJPEG at ``http://<host>:81/stream``. What this file knows of it is ported
here, not imported: eve must not depend on that checkout's path. Each piece names its source.

* A frame is compact JSON with a string ``"H"`` tag first (tools/car.py ``Car.send``). Drive is the stock timed move
  ``{"H":"<n>","N":2,"D1":<dir>,"D2":<speed>,"T":<ms>}`` with direction 1 left, 2 right, 3 forward, 4 back
  (tools/car.py ``DIR``; README "Wire protocol"). Stop is ``{"H":"<n>","N":100}``, and the UNO answers it ``{ok}``.
* The camera board sends ``{Heartbeat}`` once a second and drops a client that misses three; the client answers
  ``{Heartbeat}`` (tools/car.py ``Car._reader``; firmware/camera_s3/camera_s3.ino).
* It serves ONE client at a time. A second connection is accepted by the network stack but never read until the
  first one leaves (camera_s3.ino ``SocketServer_Test``), so a busy car is a connection that answers nothing. Eve
  sends a stop as its hello and waits HELLO_SECS for any frame back; silence means another controller has the car.
* Its video serves one viewer at a time (tools/controller.py; README "Browser controller"). JPEGs are cut out of the
  stream between the start and end markers, as tools/follow.py ``Stream`` does.

The firmware's own deadman. ``N=2`` is a timed move: the UNO stops by itself ``T`` ms after the last drive frame
(firmware/uno_v4_mod ``CMD_CarControlTimeLimit_xxx0``), and the camera board sends the UNO a stop when its client
disconnects. Eve never sends the untimed moves (``N=3``, or ``N=4`` wheel speeds with their 1.5 s deadline), so if
the Mac or the network dies mid-drive the car stops within PULSE_MS.

Safety, on top of that. Hold to drive only:

* While a direction is held the app sends ``{"action": "drive", "dir", "hold"}`` about every 150 ms. ``hold`` is one
  id per press. Each beat renews the timed move (at most every RESEND_SECS) and the heartbeat deadline.
* A stop is sent on release (``{"action": "stop", "hold"}``), when no beat came for HEARTBEAT_SECS, when the panel
  closes (``{"action": "close"}``), when the car's connection fails, and on any error. A stop is written to the
  socket at once: nothing is ever queued in front of it, and no lock is waited on.
* A press that was stopped stays stopped: a late beat of an ended hold (the phone's requests cross in the tunnel)
  is refused, never restarts the car. The owner lifts and presses again.

The slot. The car takes one controller, so eve holds it only while the panel is used: it connects on the first
drive, lets go IDLE_SECS after the last command with nothing held, and at once on ``close``. The video is the same:
one viewer, the newest wins, and ``close`` or the app hanging up ends it.

Config. ``~/.config/eve-bridge/pika.json`` (no secrets): ``{"host": "elegoo-car.local"}``, the name tools/car.py
uses (mDNS), or an address; optional ``port`` (100), ``video_port`` (81), ``speed`` (120) and ``turn_speed`` (140)
(calibration.json's ``forward_speed`` and ``turn_speed``). It is off unless ``EVE_PIKA=1`` and the file is there;
then the Apps tab has no Pika. The log keeps actions and outcomes, never the address.
"""

from __future__ import annotations

import asyncio
import collections
import contextlib
import json
import logging
import os
import re
import socket
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any, AsyncIterator, Awaitable, Callable, Optional

log = logging.getLogger(__name__)

CONFIG_PATH = Path("~/.config/eve-bridge/pika.json")
DEFAULT_HOST = "elegoo-car.local"     # tools/car.py --host default
DEFAULT_PORT = 100                    # the camera board's JSON relay (tools/car.py)
DEFAULT_VIDEO_PORT = 81               # its MJPEG server (tools/controller.py --video-port)
DEFAULT_SPEED = 120                   # calibration.json forward_speed; tools/car.py's default speed
DEFAULT_TURN_SPEED = 140              # calibration.json turn_speed
DEFAULT_STUN = "stun:stun.cloudflare.com:3478"   # pika_rtc.py: the Mac's public address, for a phone off the home Wi-Fi

DIRS = {"left": 1, "right": 2, "forward": 3, "back": 4}   # tools/car.py DIR (its "fwd" is "forward" here)
PULSE_MS = 450                        # each drive frame's timed move: the firmware deadman (tools/safety.py
                                      # MAX_PULSE_MS; tools/controller.html's held pulse)
RESEND_SECS = 0.2                     # a held direction's frame is renewed at most this often
HEARTBEAT_SECS = 0.45                 # no beat from the app for this long: stop
IDLE_SECS = 5.0                       # nothing held and no command for this long: let go of the car's slot
HELLO_SECS = 1.5                      # after connecting, how long the car gets to answer the hello stop
CONNECT_SECS = 3.0
TICK_SECS = 0.05                      # the watchdog's period
ENDED_KEPT = 128                      # ended holds remembered, so their late beats are refused
MAX_HOLD_CHARS = 64

VIDEO_HEAD_SECS = 4.0                 # the car's answer to the video request
VIDEO_FRAME_SECS = 5.0                # no new frame for this long: the video ended
MAX_VIDEO_BUFFER = 1 << 20            # bytes kept while looking for a JPEG's end
VIDEO_HEAD = (b"HTTP/1.1 200 OK\r\nContent-Type: multipart/x-mixed-replace; boundary=frame\r\n"
              b"Cache-Control: no-store\r\nX-Accel-Buffering: no\r\nTransfer-Encoding: chunked\r\n"
              b"Connection: close\r\n\r\n")

BUSY_LINE = "Pika is busy: another controller has it. Close that one and try again."
UNREACHABLE_LINE = "I can't reach Pika. Is it on, and on the home Wi-Fi?"
LOST_LINE = "Lost the connection to Pika. It stopped."
LATE_LINE = "Stopped: the phone's signal came late. Press again to drive."
VIDEO_BUSY_LINE = "Pika's video is in use by another viewer."
VIDEO_UNREACHABLE_LINE = "I can't reach Pika's camera. Is it on, and on the home Wi-Fi?"

HOST_RE = re.compile(r"^[A-Za-z0-9.-]{1,253}$")
STUN_RE = re.compile(r"^stun:[A-Za-z0-9.-]{1,253}(:[0-9]{1,5})?$")
STAT_PATHS = ("webrtc", "tunnel")

Opener = Callable[[str, int], Awaitable[tuple[asyncio.StreamReader, asyncio.StreamWriter]]]


@dataclass(frozen=True)
class PikaConfig:
    host: str = DEFAULT_HOST
    port: int = DEFAULT_PORT
    video_port: int = DEFAULT_VIDEO_PORT
    speed: int = DEFAULT_SPEED
    turn_speed: int = DEFAULT_TURN_SPEED
    rtc: bool = True                  # offer the video over WebRTC (pika_rtc.py); false: the tunnel stream only
    stun: str = DEFAULT_STUN          # "" for none: then WebRTC works on the home Wi-Fi only


def enabled(environ: Any = None) -> bool:
    """Off unless EVE_PIKA is on: a car that moves is never offered by default."""
    env = os.environ if environ is None else environ
    return str(env.get("EVE_PIKA", "")).strip().lower() in ("1", "true", "yes", "on")


def load(path: Optional[Path] = None) -> Optional[PikaConfig]:
    """The config file, or None when it is missing or not a JSON object. Bad values fall back to the defaults."""
    p = (path or CONFIG_PATH).expanduser()
    try:
        raw = json.loads(p.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None
    if not isinstance(raw, dict):
        return None

    def number(key: str, default: int, lo: int, hi: int) -> int:
        v = raw.get(key, default)
        return v if isinstance(v, int) and not isinstance(v, bool) and lo <= v <= hi else default

    host = raw.get("host", DEFAULT_HOST)
    if not isinstance(host, str) or not HOST_RE.match(host.strip()):
        host = DEFAULT_HOST
    rtc = raw.get("rtc", True)
    stun = raw.get("stun", DEFAULT_STUN)
    if not isinstance(stun, str) or not (stun == "" or STUN_RE.match(stun)):
        stun = DEFAULT_STUN
    return PikaConfig(host=host.strip(), port=number("port", DEFAULT_PORT, 1, 65535),
                      rtc=rtc if isinstance(rtc, bool) else True, stun=stun,
                      video_port=number("video_port", DEFAULT_VIDEO_PORT, 1, 65535),
                      speed=number("speed", DEFAULT_SPEED, 0, 255),
                      turn_speed=number("turn_speed", DEFAULT_TURN_SPEED, 0, 255))


def make_pika(path: Optional[Path] = None, environ: Any = None) -> Optional["Pika"]:
    """The car's panel, or None when EVE_PIKA is off or there is no config (then the app shows no Pika). Making it
    sends nothing: the first drive connects."""
    if not enabled(environ):
        log.info("pika: off (EVE_PIKA)")
        return None
    cfg = load(path)
    if cfg is None:
        log.info("pika: no config (%s)", CONFIG_PATH)
        return None
    car = Pika(cfg)
    from . import pika_rtc
    if cfg.rtc and pika_rtc.available():
        car.rtc = pika_rtc.PikaRtc(car, cfg.stun)
    log.info("pika: on (video over %s)", "webrtc, tunnel fallback" if car.rtc else "the tunnel")
    return car


# ---- the wire (tools/car.py) ---------------------------------------------------------------------------------

def frame(tag: int, **fields: int) -> bytes:
    """One frame as tools/car.py ``Car.send`` writes it: ``{"H":"<tag>",...}``, compact."""
    return json.dumps({"H": str(tag), **fields}, separators=(",", ":")).encode()


def drive_frame(tag: int, direction: int, speed: int, ms: int = PULSE_MS) -> bytes:
    return frame(tag, N=2, D1=direction, D2=speed, T=ms)


def stop_frame(tag: int) -> bytes:
    return frame(tag, N=100)


HEARTBEAT = b"{Heartbeat}"


def split_frames(buf: str) -> tuple[list[str], str]:
    """Complete ``{...}`` frames in ``buf`` and what is left (tools/car.py ``Car._reader``)."""
    out = []
    while "}" in buf:
        part, buf = buf.split("}", 1)
        part += "}"
        out.append(part[part.rfind("{"):] if "{" in part else part)
    return out, buf


# ---- the video (tools/follow.py Stream) ----------------------------------------------------------------------

def split_jpegs(buf: bytes) -> tuple[list[bytes], bytes]:
    """Whole JPEGs (start marker to end marker) in ``buf``, and the bytes to keep for the next read."""
    out = []
    while True:
        a = buf.find(b"\xff\xd8")
        z = buf.find(b"\xff\xd9", a + 2) if a >= 0 else -1
        if a < 0 or z < 0:
            return out, (buf[a:] if a >= 0 else buf[-1:])
        out.append(buf[a:z + 2])
        buf = buf[z + 2:]


def stamp_ms() -> int:
    """The Mac's wall clock in ms, put on every frame so the phone can measure how late it arrives."""
    return int(time.time() * 1000)


def video_part(jpeg: bytes, stamp: Optional[int] = None) -> bytes:
    """One part of the app's multipart stream, stamped with when it left the Mac."""
    return (b"--frame\r\nContent-Type: image/jpeg\r\nContent-Length: %d\r\nX-Stamp: %d\r\n\r\n"
            % (len(jpeg), stamp_ms() if stamp is None else stamp) + jpeg + b"\r\n")


async def _body(reader: asyncio.StreamReader, chunked: bool) -> AsyncIterator[bytes]:
    """The response body's bytes: de-chunked when the camera's server sends it chunked (ESP-IDF's httpd does)."""
    if not chunked:
        while True:
            data = await reader.read(16384)
            if not data:
                return
            yield data
    while True:
        size_line = await reader.readuntil(b"\r\n")
        size = int(size_line.split(b";", 1)[0].strip() or b"0", 16)
        if size == 0:
            return
        yield await reader.readexactly(size)
        await reader.readexactly(2)


async def _open(host: str, port: int) -> tuple[asyncio.StreamReader, asyncio.StreamWriter]:
    # IPv4 only: the car has no IPv6, and macOS spends 5 s on the mDNS AAAA lookup before giving up, longer than
    # CONNECT_SECS. Measured 2026-10-10: getaddrinfo("elegoo-car.local") 5.06 s, with AF_INET 0.02 s.
    return await asyncio.open_connection(host, port, family=socket.AF_INET)


async def _chunk(writer: asyncio.StreamWriter, data: bytes) -> None:
    writer.write(f"{len(data):x}\r\n".encode() + data + b"\r\n")
    await writer.drain()


async def _json(writer: asyncio.StreamWriter, status: int, obj: dict[str, Any]) -> None:
    body = json.dumps(obj).encode()
    reason = {200: "OK", 503: "Service Unavailable"}.get(status, "OK")
    writer.write(f"HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {len(body)}\r\n"
                 "Cache-Control: no-store\r\nConnection: close\r\n\r\n".encode() + body)
    await writer.drain()


class PikaError(Exception):
    """A plain line for the owner."""


# ---- the panel -----------------------------------------------------------------------------------------------

class Pika:
    """The Pika screen's ``panel(body)`` and video ``stream(writer)``. One per daemon, on the event loop."""

    available = True

    def __init__(self, cfg: PikaConfig, *, opener: Opener = _open, clock: Callable[[], float] = time.monotonic,
                 heartbeat_secs: float = HEARTBEAT_SECS, idle_secs: float = IDLE_SECS,
                 hello_secs: float = HELLO_SECS, tick_secs: float = TICK_SECS) -> None:
        self.cfg = cfg
        self._open = opener
        self._clock = clock
        self.heartbeat_secs = heartbeat_secs
        self.idle_secs = idle_secs
        self.hello_secs = hello_secs
        self.tick_secs = tick_secs
        self._reader: Optional[asyncio.StreamReader] = None
        self._writer: Optional[asyncio.StreamWriter] = None
        self._rx: Optional[asyncio.Task] = None
        self._watch: Optional[asyncio.Task] = None
        self._connecting: Optional[asyncio.Task] = None
        self._answered: Optional[asyncio.Event] = None
        self._tag = 0
        self._held: Optional[str] = None           # the hold driving now
        self._dir = 0
        self._deadline = 0.0                       # the held direction's heartbeat deadline
        self._sent_at = 0.0                        # when its frame was last written
        self._used = 0.0                           # the last command
        self._epoch = 0                            # bumped by every stop: a drive that waited on a connect re-checks
        self._ended: collections.OrderedDict[str, None] = collections.OrderedDict()
        self._note = ""                            # why the last hold was stopped, said to its next beat
        self._video: Optional[asyncio.Task] = None
        self.stops: list[str] = []                 # why each stop was sent (the tests read it)
        self.rtc: Any = None                       # pika_rtc.PikaRtc: the video over WebRTC, when aiortc is there

    # -- state --
    @property
    def connected(self) -> bool:
        return self._writer is not None and not self._writer.is_closing()

    def _next_tag(self) -> int:
        self._tag = self._tag % 99999 + 1
        return self._tag

    def _end(self, hold: Optional[str]) -> None:
        if hold:
            self._ended[hold] = None
            while len(self._ended) > ENDED_KEPT:
                self._ended.popitem(last=False)

    # -- the panel --
    async def panel(self, body: dict[str, Any]) -> dict[str, Any]:
        action = str(body.get("action") or "status")
        hold = body.get("hold")
        hold = hold.strip()[:MAX_HOLD_CHARS] if isinstance(hold, str) else ""
        if action == "status":
            return {"ok": True, "connected": self.connected, "rtc": self.rtc is not None, "line": ""}
        if action == "rtc":
            return await self._rtc(body.get("sdp"))
        if action == "stats":
            return self._stats(body)
        if action == "stop":
            self._end(hold)
            self.stop("release")
            self._used = self._clock()
            return {"ok": True, "line": ""}
        if action == "close":
            self._end(hold)
            await self.close("panel closed")
            return {"ok": True, "line": ""}
        if action != "drive":
            return {"ok": False, "line": "That isn't something Pika does."}
        direction = DIRS.get(str(body.get("dir") or ""))
        if direction is None or not hold:
            return {"ok": False, "line": "Which way?"}
        try:
            return await self._drive(hold, direction)
        except PikaError as e:
            self._end(hold)
            self.stop("error")
            return {"ok": False, "stopped": True, "line": str(e)}
        except (OSError, ConnectionError) as e:
            log.warning("pika: drive failed (%s)", type(e).__name__)
            self._end(hold)
            self.stop("error")
            self._drop()
            return {"ok": False, "stopped": True, "line": LOST_LINE}

    async def _rtc(self, sdp: Any) -> dict[str, Any]:
        """Answer the app's WebRTC offer for the video (pika_rtc.py). ``rtc: false``: use the tunnel stream."""
        if self.rtc is None:
            return {"ok": False, "rtc": False, "line": ""}
        if not isinstance(sdp, str) or not sdp.startswith("v=0") or len(sdp) > 20000:
            return {"ok": False, "rtc": False, "line": ""}
        try:
            answer = await asyncio.wait_for(self.rtc.answer(sdp), 8.0)
        except Exception as e:                                    # aiortc raises its own kinds; the tunnel still works
            log.info("pika: webrtc answer failed (%s)", type(e).__name__)
            return {"ok": False, "rtc": False, "line": ""}
        return {"ok": True, "rtc": True, "sdp": answer, "line": ""}

    def _stats(self, body: dict[str, Any]) -> dict[str, Any]:
        """The phone's latency summary for one video path: logged, so the two paths can be compared."""
        path = body.get("path")

        def num(key: str) -> Optional[float]:
            v = body.get(key)
            return float(v) if isinstance(v, (int, float)) and not isinstance(v, bool) and abs(v) < 1e7 else None

        n, p50, p95, fps = num("n"), num("p50"), num("p95"), num("fps")
        if path not in STAT_PATHS or None in (n, p50, p95, fps):
            return {"ok": False, "line": ""}
        log.info("pika: video stats %s n=%d p50=%.0f p95=%.0f fps=%.1f", path, n, p50, p95, fps)
        return {"ok": True, "line": ""}

    async def _drive(self, hold: str, direction: int) -> dict[str, Any]:
        if hold in self._ended:
            return {"ok": False, "stopped": True, "line": self._note}
        epoch = self._epoch
        self._used = self._clock()
        if not self.connected:
            await self._connect()
        if epoch != self._epoch or hold in self._ended:      # a stop came while connecting: this press is over
            self._end(hold)
            return {"ok": False, "stopped": True, "line": self._note}
        now = self._clock()
        if self._held != hold:
            self._end(self._held)                             # a new press replaces the old one
            self._held, self._dir, self._sent_at = hold, direction, 0.0
            self._note = ""
        elif self._dir != direction:
            self._dir, self._sent_at = direction, 0.0
        self._deadline = now + self.heartbeat_secs
        self._used = now
        if now - self._sent_at >= RESEND_SECS:
            speed = self.cfg.speed if direction in (DIRS["forward"], DIRS["back"]) else self.cfg.turn_speed
            self._write(drive_frame(self._next_tag(), direction, speed))
            self._sent_at = now
        return {"ok": True, "line": ""}

    # -- stop: written at once, never behind anything --
    def stop(self, why: str) -> None:
        """Stop now. Ends the held press and writes a stop if the car is connected; never awaits."""
        self._epoch += 1
        if self._held is not None:
            self._end(self._held)
            self._held, self._dir = None, 0
        if self.connected:
            self.stops.append(why)
            try:
                self._write(stop_frame(self._next_tag()))
            except (OSError, ConnectionError):
                self._drop()
            log.info("pika: stop (%s)", why)

    def _write(self, data: bytes) -> None:
        if self._writer is None or self._writer.is_closing():
            raise ConnectionError("not connected")
        self._writer.write(data)

    # -- the connection --
    async def _connect(self) -> None:
        if self._connecting is None or self._connecting.done():
            self._connecting = asyncio.ensure_future(self._dial())
        await asyncio.shield(self._connecting)

    async def _dial(self) -> None:
        try:
            reader, writer = await asyncio.wait_for(self._open(self.cfg.host, self.cfg.port), CONNECT_SECS)
        except (OSError, asyncio.TimeoutError) as e:
            log.info("pika: car unreachable (%s %s)", type(e).__name__, getattr(e, "errno", None) or "")
            raise PikaError(UNREACHABLE_LINE) from None
        self._reader, self._writer = reader, writer
        self._answered = asyncio.Event()
        self._rx = asyncio.ensure_future(self._read(reader, writer))
        try:
            self._write(stop_frame(self._next_tag()))        # the hello: harmless, and the UNO answers it
            await asyncio.wait_for(self._answered.wait(), self.hello_secs)
        except asyncio.TimeoutError:
            log.info("pika: the car did not answer (another controller has it)")
            self._drop()
            raise PikaError(BUSY_LINE) from None
        except (OSError, ConnectionError):
            self._drop()
            raise PikaError(UNREACHABLE_LINE) from None
        if self._watch is None or self._watch.done():
            self._watch = asyncio.ensure_future(self._watchdog())
        log.info("pika: connected")

    async def _read(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        buf = ""
        try:
            while True:
                data = await reader.read(1024)
                if not data:
                    break
                frames, buf = split_frames(buf + data.decode("utf-8", "replace"))
                if len(buf) > 4096:
                    buf = ""
                for f in frames:
                    if self._answered is not None:
                        self._answered.set()
                    if f == "{Heartbeat}" and not writer.is_closing():
                        writer.write(HEARTBEAT)
        except (OSError, ConnectionError, asyncio.IncompleteReadError):
            pass
        if self._writer is writer:
            # the car is gone; its camera board stops the UNO when its client leaves (camera_s3.ino)
            log.info("pika: the car's connection ended")
            self._note = LOST_LINE if self._held else self._note
            self._end(self._held)
            self._held = None
            self._epoch += 1
            self._drop()

    async def _watchdog(self) -> None:
        while self.connected:
            await asyncio.sleep(self.tick_secs)
            now = self._clock()
            if self._held is not None and now > self._deadline:
                self._note = LATE_LINE
                self.stop("heartbeat")
            elif self._held is None and now - self._used > self.idle_secs:
                log.info("pika: idle, letting go of the car")
                self.stop("idle")
                await self._release()
                return

    def _drop(self) -> None:
        writer, self._writer, self._reader = self._writer, None, None
        if writer is not None:
            with contextlib.suppress(Exception):
                writer.close()
        rx, self._rx = self._rx, None
        if rx is not None and rx is not asyncio.current_task():
            rx.cancel()

    async def _release(self) -> None:
        writer = self._writer
        if writer is not None:
            with contextlib.suppress(Exception):
                await asyncio.wait_for(writer.drain(), 0.5)     # the stop goes out before the socket closes
        self._drop()

    async def close(self, why: str = "closed") -> None:
        """Stop, let go of the car's slot and end the video (the panel closed, or the daemon stops)."""
        self.stop(why)
        await self._release()
        watch, self._watch = self._watch, None
        if watch is not None and watch is not asyncio.current_task():
            watch.cancel()
        await self._end_video()
        if self.rtc is not None:
            await self.rtc.close()

    # -- the video --
    async def frames(self) -> AsyncIterator[bytes]:
        """The car's camera as whole JPEGs, the newest only: a reader that falls behind skips frames, never queues
        them. Raises PikaError with the owner's line when the camera can't be reached or is busy; ends when the
        camera ends or goes quiet for VIDEO_FRAME_SECS. The caller holds the one viewer slot (``_take_video``)."""
        up_writer: Optional[asyncio.StreamWriter] = None
        try:
            try:
                up_reader, up_writer = await asyncio.wait_for(self._open(self.cfg.host, self.cfg.video_port),
                                                              CONNECT_SECS)
                up_writer.write(f"GET /stream HTTP/1.1\r\nHost: {self.cfg.host}\r\nConnection: close\r\n\r\n".encode())
                await up_writer.drain()
                head = await asyncio.wait_for(up_reader.readuntil(b"\r\n\r\n"), VIDEO_HEAD_SECS)
            except asyncio.TimeoutError:
                raise PikaError(VIDEO_BUSY_LINE if up_writer else VIDEO_UNREACHABLE_LINE) from None
            except (OSError, ConnectionError, asyncio.IncompleteReadError, asyncio.LimitOverrunError) as e:
                log.info("pika: camera unreachable (%s %s)", type(e).__name__, getattr(e, "errno", None) or "")
                raise PikaError(VIDEO_UNREACHABLE_LINE) from None
            lines = head.decode("latin-1").split("\r\n")
            if len(lines[0].split()) < 2 or lines[0].split()[1] != "200":
                raise PikaError(VIDEO_BUSY_LINE)
            chunked = any(ln.lower().startswith("transfer-encoding:") and "chunked" in ln.lower() for ln in lines)
            latest: list[bytes] = []
            fresh = asyncio.Event()

            async def pull() -> None:
                buf = b""
                async for data in _body(up_reader, chunked):
                    jpegs, buf = split_jpegs(buf + data)
                    if len(buf) > MAX_VIDEO_BUFFER:
                        buf = b""
                    if jpegs:
                        latest[:] = [jpegs[-1]]
                        fresh.set()

            puller = asyncio.ensure_future(pull())
            try:
                while True:
                    waiter = asyncio.ensure_future(fresh.wait())
                    done, _ = await asyncio.wait({waiter, puller}, timeout=VIDEO_FRAME_SECS,
                                                 return_when=asyncio.FIRST_COMPLETED)
                    if waiter not in done:
                        waiter.cancel()
                        return                                  # the camera ended, failed or went quiet
                    fresh.clear()
                    if latest:
                        yield latest.pop()
            finally:
                puller.cancel()
                with contextlib.suppress(BaseException):
                    await puller
        finally:
            if up_writer is not None:
                with contextlib.suppress(Exception):
                    up_writer.close()

    async def _take_video(self, keep_rtc: bool = False) -> None:
        """The camera serves one viewer: the newest, over either path, ends the one before it."""
        await self._end_video()
        if self.rtc is not None and not keep_rtc:
            await self.rtc.close()
        self._video = asyncio.current_task()

    def _left_video(self) -> None:
        if self._video is asyncio.current_task():
            self._video = None
        log.info("pika: video off")

    async def stream(self, writer: asyncio.StreamWriter) -> None:
        """The car's video to the app as multipart JPEGs through the tunnel, each part stamped ``X-Stamp`` with the
        Mac's clock in ms (the phone measures latency with it). WebRTC's fallback (pika_rtc.py)."""
        await self._take_video()
        try:
            frames = self.frames()
            try:
                first = await anext(frames)
            except PikaError as e:
                await _json(writer, 503, {"ok": False, "line": str(e)})
                return
            except StopAsyncIteration:
                await _json(writer, 503, {"ok": False, "line": VIDEO_BUSY_LINE})
                return
            writer.write(VIDEO_HEAD)
            await writer.drain()
            log.info("pika: video on (tunnel)")
            await _chunk(writer, video_part(first))
            try:
                async for jpeg in frames:
                    await _chunk(writer, video_part(jpeg))
            finally:
                await frames.aclose()
            with contextlib.suppress(Exception):
                await _chunk(writer, b"")
        except (OSError, ConnectionError):
            pass                                                # the app hung up
        finally:
            self._left_video()

    async def _end_video(self) -> None:
        task, self._video = self._video, None
        if task is not None and task is not asyncio.current_task() and not task.done():
            task.cancel()
            with contextlib.suppress(BaseException):
                await task
