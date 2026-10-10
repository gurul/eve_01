"""Host vision: face detection on the Mac for the board's camera frames.

The board streams small camera frames over the serial link and the daemon
answers each one with where the largest face is, so the robot can turn to
look at the person. The Mac does the detection because the board's own
detector is slow and coarse; on-board tracking is only the fallback for
when no host is attached.

Wire contract (fixed; the firmware side is built against it):

    host -> board  {"cmd":"cam","on":true,"fps":5,"w":160,"h":120}
    host -> board  {"cmd":"cam","on":false}
    board -> host  {"frame":{"seq":n,"w":160,"h":120,"fmt":"jpeg"|"gray",
                             "b64":"...","yaw":<deg>,"pitch":<deg>}}
    host -> board  {"cmd":"face","seq":n,"bx":-100..100,"by":-100..100,
                    "size":0..100,"conf":0..100,"yaw":<echo>,"pitch":<echo>,
                    "who":"unknown"}
    host -> board  {"cmd":"face","seq":n,"conf":0,"who":"unknown"}   (no face)

``who`` is the identity slot: "owner" when identity.py matched the largest
face against the enrolled owner prints, "unknown" otherwise.

``fmt:"gray"`` is raw 8-bit luma, w*h bytes. ``fmt:"jpeg"`` is a baseline
JPEG. ``bx``/``by`` is the face centre relative to the frame centre
(+bx = right of frame, +by = down), ``size`` is the face width as a
percentage of the frame width.

Two halves, kept apart so the geometry and the drop policy are testable
without a Mac (same shape as listen_key.py):

* Pure core — ``decode_frame``, ``pick_face``, ``build_face_cmd``,
  ``FrameRateGovernor`` and the ``FaceTracker`` driver, which takes any
  ``Detector`` callable.
* Platform adapter, darwin — ``make_detector`` wraps the macOS Vision
  framework (``VNDetectFaceRectanglesRequest``) via pyobjc. It returns None
  when Vision is unavailable and logs one warning; the daemon then never
  asks the board to stream.

Thread model: the detector runs on a single-worker ThreadPoolExecutor so
the asyncio loop never blocks on a detect. While a detect is in flight,
newer frames replace each other in a one-slot "latest" holder — the
governor never queues, so the board always gets an answer for a recent
frame rather than a backlog of stale ones.
"""

from __future__ import annotations

import asyncio
import base64
import binascii
import concurrent.futures
import logging
import os
import struct
import sys
import time
import zlib
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Awaitable, Callable, Optional, Protocol

log = logging.getLogger(__name__)

# What we ask the board to stream. 160x120 at 5 fps is ~4 KB/frame as JPEG
# and ~26 KB/frame as base64 gray — both fine on the USB link, and Vision
# finds a face at arm's length in 160 px wide frames.
CAM_FPS = 5
CAM_W = 160
CAM_H = 120

# Bench debugging: write at most one received frame per second to disk.
SAVE_FRAMES_MIN_INTERVAL = 1.0
# Stats line cadence in the daemon.
STATS_INTERVAL_SECS = 30.0

FRAME_FORMATS = ("jpeg", "gray")
# Identity slot on every face cmd. Detection only knows there is a face;
# identity.py says "owner".
WHO_UNKNOWN = "unknown"


@dataclass(frozen=True)
class Frame:
    seq: int
    w: int
    h: int
    fmt: str            # "jpeg" | "gray"
    data: bytes         # decoded payload: JPEG file bytes, or w*h luma bytes
    yaw: Optional[float] = None
    pitch: Optional[float] = None
    snap: bool = False  # a one-off full-size photo answering {"cmd":"snap"}, not a stream frame


@dataclass(frozen=True)
class Rect:
    """A detected face in top-left-origin pixel coordinates of the frame."""

    x: float
    y: float
    w: float
    h: float
    conf: float = 1.0   # 0..1


@dataclass(frozen=True)
class FaceResult:
    bx: int      # -100..100, + = right of frame centre
    by: int      # -100..100, + = below frame centre
    size: int    # 0..100, face width as % of frame width
    conf: int    # 0..100


# A detector takes a frame and returns every face it sees, in pixel coords.
# It runs on the executor thread, so it may block.
Detector = Callable[[Frame], list[Rect]]
Sender = Callable[[dict[str, Any]], Awaitable[bool]]


class Identity(Protocol):
    """identity.py's FaceIdentity: ``who`` for the largest face. Executor thread."""

    def process(self, frame: Frame, rects: list[Rect]) -> str: ...


# ---- pure core --------------------------------------------------------------

def build_cam_cmd(on: bool, fps: int = CAM_FPS, w: int = CAM_W, h: int = CAM_H) -> dict[str, Any]:
    if not on:
        return {"cmd": "cam", "on": False}
    return {"cmd": "cam", "on": True, "fps": fps, "w": w, "h": h}


def decode_frame(obj: dict[str, Any]) -> Frame:
    """Turn the ``frame`` object off the wire into a Frame.

    Raises ValueError for anything malformed: bad base64, unknown format,
    a gray payload whose length does not match w*h, or a JPEG payload
    without the SOI marker.
    """
    try:
        seq = int(obj["seq"])
        w = int(obj["w"])
        h = int(obj["h"])
    except (KeyError, TypeError, ValueError) as e:
        raise ValueError(f"frame: bad seq/w/h: {e}") from None
    if w <= 0 or h <= 0:
        raise ValueError(f"frame: bad size {w}x{h}")
    fmt = str(obj.get("fmt") or "")
    if fmt not in FRAME_FORMATS:
        raise ValueError(f"frame: unknown fmt {fmt!r}")
    b64 = obj.get("b64")
    if not isinstance(b64, str) or not b64:
        raise ValueError("frame: missing b64")
    try:
        data = base64.b64decode(b64, validate=True)
    except (binascii.Error, ValueError) as e:
        raise ValueError(f"frame: bad base64: {e}") from None
    if fmt == "gray" and len(data) != w * h:
        raise ValueError(f"frame: gray payload is {len(data)} bytes, expected {w * h}")
    if fmt == "jpeg" and data[:2] != b"\xff\xd8":
        raise ValueError("frame: jpeg payload lacks SOI marker")
    return Frame(
        seq=seq, w=w, h=h, fmt=fmt, data=data,
        yaw=_opt_float(obj.get("yaw")), pitch=_opt_float(obj.get("pitch")),
        snap=obj.get("snap") is True,
    )


def build_snap_cmd() -> dict[str, Any]:
    """Ask the board for one full-resolution photo (a frame line with ``"snap":true``)."""
    return {"cmd": "snap"}


def _opt_float(v: Any) -> Optional[float]:
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return None
    return float(v)


def _clamp(v: float, lo: int, hi: int) -> int:
    return max(lo, min(hi, int(round(v))))


def largest_rect(rects: list[Rect]) -> Rect:
    """The face we track and identify: largest by area."""
    return max(rects, key=lambda r: r.w * r.h)


def pick_face(rects: list[Rect], w: int, h: int) -> Optional[FaceResult]:
    """Largest rect (by area) -> centre offset, width %, confidence %."""
    if not rects or w <= 0 or h <= 0:
        return None
    best = largest_rect(rects)
    cx = best.x + best.w / 2.0
    cy = best.y + best.h / 2.0
    return FaceResult(
        bx=_clamp((cx - w / 2.0) / (w / 2.0) * 100.0, -100, 100),
        by=_clamp((cy - h / 2.0) / (h / 2.0) * 100.0, -100, 100),
        size=_clamp(best.w / w * 100.0, 0, 100),
        conf=_clamp(best.conf * 100.0, 0, 100),
    )


def build_face_cmd(
    seq: int,
    result: Optional[FaceResult],
    yaw: Optional[float] = None,
    pitch: Optional[float] = None,
    who: str = WHO_UNKNOWN,
) -> dict[str, Any]:
    """The reply for one processed frame. ``conf:0`` without bx/by means "seen, no face"."""
    if result is None:
        return {"cmd": "face", "seq": seq, "conf": 0, "who": who}
    cmd: dict[str, Any] = {
        "cmd": "face", "seq": seq,
        "bx": result.bx, "by": result.by, "size": result.size, "conf": result.conf,
    }
    if yaw is not None:
        cmd["yaw"] = yaw
    if pitch is not None:
        cmd["pitch"] = pitch
    cmd["who"] = who
    return cmd


class FrameRateGovernor:
    """Drop policy: run one detect at a time, keep only the newest waiting frame.

    ``offer`` returns the frame to run now, or None when a detect is busy —
    in which case the frame is parked as the single pending one, displacing
    (and counting as dropped) whatever was parked before. ``done`` marks the
    detect finished and hands back the parked frame to run next, or None to
    go idle. Nothing is ever queued deeper than one.
    """

    def __init__(self) -> None:
        self.busy = False
        self.pending: Optional[Frame] = None
        self.received = 0
        self.processed = 0
        self.dropped = 0

    def offer(self, frame: Frame) -> Optional[Frame]:
        self.received += 1
        if self.busy:
            if self.pending is not None:
                self.dropped += 1
            self.pending = frame
            return None
        self.busy = True
        return frame

    def done(self) -> Optional[Frame]:
        self.processed += 1
        nxt, self.pending = self.pending, None
        if nxt is None:
            self.busy = False
        return nxt


@dataclass
class _Window:
    """Counters at the start of the current stats window."""

    at: float
    received: int = 0
    dropped: int = 0
    processed: int = 0
    faces: int = 0
    detect_secs: float = 0.0


class FaceTracker:
    """Drives frames through the governor, the detector and back to the board.

    ``detect`` is None when no detector is available (not macOS, Vision
    missing): ``enabled`` is False, the daemon never sends ``cam on`` and
    any frame that still arrives is ignored. ``send`` is the transport's
    send coroutine. ``save_dir`` writes every received frame, rate-limited
    to one per second, for bench debugging. ``identity`` (identity.py's
    FaceIdentity, or anything with ``process(frame, rects) -> who``) runs
    on the same executor thread straight after the detect and fills the
    face cmd's ``who``; None leaves every face "unknown".
    """

    def __init__(
        self,
        detect: Optional[Detector],
        send: Sender,
        save_dir: Optional[Path] = None,
        clock: Callable[[], float] = time.monotonic,
        identity: Optional[Identity] = None,
        on_faces: Optional[Callable[[list[FaceResult], Optional[float], Optional[float]], Awaitable[None]]] = None,
    ) -> None:
        self.detect = detect
        self.send = send
        # Every face of every processed frame, with the pose the frame was taken at — for the
        # conversation's follower (follow.py). The board still gets only the largest, as the wire says.
        self.on_faces = on_faces
        self.save_dir = save_dir
        self.clock = clock
        self.identity = identity
        self.governor = FrameRateGovernor()
        self.faces = 0                  # processed frames that had a face
        self.detect_secs = 0.0          # summed wall time inside the executor
        self.bad_frames = 0
        self._first_logged = False
        self._last_saved = float("-inf")
        self._saved = 0
        self._window = _Window(at=clock())
        self._executor: Optional[concurrent.futures.ThreadPoolExecutor] = None
        self._task: Optional[asyncio.Task[None]] = None

    @property
    def enabled(self) -> bool:
        return self.detect is not None

    def stop(self) -> None:
        if self._executor is not None:
            self._executor.shutdown(wait=False, cancel_futures=True)
            self._executor = None

    async def on_frame(self, obj: dict[str, Any]) -> None:
        """Loop thread. Decode, maybe save, then run or park the frame."""
        if not self.enabled:
            return
        try:
            frame = decode_frame(obj)
        except ValueError as e:
            self.bad_frames += 1
            if self.bad_frames == 1:
                log.warning("vision: bad frame from board — %s (further ones counted, not logged)", e)
            return
        if not self._first_logged:
            self._first_logged = True
            log.info("vision: first frame %dx%d %s %.1f KB",
                     frame.w, frame.h, frame.fmt, len(frame.data) / 1024.0)
        self._maybe_save(frame)
        run_now = self.governor.offer(frame)
        if run_now is not None:
            self._task = asyncio.ensure_future(self._run(run_now))

    async def _run(self, frame: Frame) -> None:
        """Detect on the executor, reply, then drain the governor's parked frame."""
        loop = asyncio.get_running_loop()
        if self._executor is None:
            self._executor = concurrent.futures.ThreadPoolExecutor(
                max_workers=1, thread_name_prefix="vision")
        nxt: Optional[Frame] = frame
        while nxt is not None:
            cur = nxt
            t0 = self.clock()
            try:
                rects, who = await loop.run_in_executor(self._executor, self._work, cur)
            except RuntimeError:
                # Executor shut down under us (daemon stopping).
                return
            except Exception:  # noqa: BLE001
                log.exception("vision: detector failed on frame %d", cur.seq)
                rects, who = [], WHO_UNKNOWN
            self.detect_secs += self.clock() - t0
            result = pick_face(rects, cur.w, cur.h)
            if result is not None:
                self.faces += 1
            await self.send(build_face_cmd(cur.seq, result, cur.yaw, cur.pitch, who=who))
            if self.on_faces is not None:
                try:
                    each = [f for f in (pick_face([r], cur.w, cur.h) for r in rects) if f is not None]
                    await self.on_faces(each, cur.yaw, cur.pitch)
                except Exception:  # noqa: BLE001 — a follower that fails must never stop the tracker
                    log.exception("vision: on_faces failed on frame %d", cur.seq)
            nxt = self.governor.done()

    def _work(self, frame: Frame) -> tuple[list[Rect], str]:
        """Executor thread: detect, then identify the largest face on the same
        thread so two Vision requests never run at once."""
        assert self.detect is not None
        rects = self.detect(frame)
        who = WHO_UNKNOWN
        if rects and self.identity is not None:
            who = self.identity.process(frame, rects)
        return rects, who

    # ---- stats ----

    def stats_line(self) -> Optional[str]:
        """The once-per-window summary, or None when no frame arrived. Resets the window."""
        now = self.clock()
        g = self.governor
        w = self._window
        received = g.received - w.received
        self._window = _Window(
            at=now, received=g.received, dropped=g.dropped,
            processed=g.processed, faces=self.faces, detect_secs=self.detect_secs,
        )
        if received == 0:
            return None
        dropped = g.dropped - w.dropped
        processed = g.processed - w.processed
        faces = self.faces - w.faces
        secs = max(now - w.at, 1e-6)
        ms = (self.detect_secs - w.detect_secs) / processed * 1000.0 if processed else 0.0
        pct = 100.0 * faces / processed if processed else 0.0
        return (f"vision: {received} frames, {dropped} dropped, {received / secs:.1f} fps, "
                f"{ms:.0f} ms/detect, faces {pct:.0f}%")

    # ---- bench: frame dump ----

    def _maybe_save(self, frame: Frame) -> None:
        if self.save_dir is None:
            return
        now = self.clock()
        if now - self._last_saved < SAVE_FRAMES_MIN_INTERVAL:
            return
        self._last_saved = now
        try:
            path = save_frame(frame, self.save_dir)
        except OSError as e:
            log.warning("vision: could not save frame to %s: %s — disabling frame dump", self.save_dir, e)
            self.save_dir = None
            return
        self._saved += 1
        if self._saved == 1:
            log.info("vision: saving frames to %s (first: %s)", self.save_dir, path.name)


def save_frame(frame: Frame, directory: Path) -> Path:
    """Write the frame as it arrived: JPEG bytes verbatim, gray as an 8-bit PNG."""
    directory.mkdir(parents=True, exist_ok=True)
    if frame.fmt == "jpeg":
        path = directory / f"frame_{frame.seq:06d}.jpg"
        path.write_bytes(frame.data)
    else:
        path = directory / f"frame_{frame.seq:06d}.png"
        path.write_bytes(encode_gray_png(frame.w, frame.h, frame.data))
    return path


def encode_gray_png(w: int, h: int, luma: bytes) -> bytes:
    """Minimal PNG writer for 8-bit grayscale (stdlib only; no PIL dependency)."""
    if len(luma) != w * h:
        raise ValueError(f"luma is {len(luma)} bytes, expected {w * h}")

    def chunk(tag: bytes, body: bytes) -> bytes:
        return (struct.pack(">I", len(body)) + tag + body
                + struct.pack(">I", zlib.crc32(tag + body) & 0xFFFFFFFF))

    raw = b"".join(b"\x00" + luma[y * w:(y + 1) * w] for y in range(h))  # filter 0 per row
    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 0, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(raw))
            + chunk(b"IEND", b""))


def configured_save_dir(cli_value: Optional[str] = None) -> Optional[Path]:
    """``--save-frames DIR`` wins, else ``EVE_SAVE_FRAMES``; None when unset."""
    raw = cli_value or os.environ.get("EVE_SAVE_FRAMES") or ""
    raw = raw.strip()
    return Path(raw).expanduser() if raw else None


# ---- platform adapter: macOS Vision ------------------------------------------

def make_detector() -> Optional[Detector]:
    """The Vision-framework detector, or None (with one warning) when unavailable."""
    if sys.platform != "darwin":
        log.warning("vision: host face tracking needs macOS Vision — board keeps on-board tracking")
        return None
    try:
        import Quartz  # noqa: F401
        import Vision  # noqa: F401
        from Foundation import NSData  # noqa: F401
    except ImportError as e:
        log.warning(
            "vision: pyobjc Vision/Quartz not importable (%s) — board keeps on-board "
            "tracking (pip install pyobjc-framework-Vision pyobjc-framework-Quartz)", e)
        return None
    return vision_detect


def _cgimage_from_frame(frame: Frame) -> Any:
    """Build a CGImage from the frame bytes. Returns None when decoding fails."""
    import Quartz

    if frame.fmt == "gray":
        provider = Quartz.CGDataProviderCreateWithData(None, frame.data, len(frame.data), None)
        cs = Quartz.CGColorSpaceCreateDeviceGray()
        return Quartz.CGImageCreate(
            frame.w, frame.h, 8, 8, frame.w, cs, Quartz.kCGImageAlphaNone,
            provider, None, False, Quartz.kCGRenderingIntentDefault)
    from Foundation import NSData

    nsdata = NSData.dataWithBytes_length_(frame.data, len(frame.data))
    source = Quartz.CGImageSourceCreateWithData(nsdata, None)
    if source is None:
        return None
    return Quartz.CGImageSourceCreateImageAtIndex(source, 0, None)


def vision_detect(frame: Frame) -> list[Rect]:
    """Executor thread. VNDetectFaceRectanglesRequest over a CGImage of the frame.

    Vision reports each face as a normalized boundingBox with its origin at
    the BOTTOM-LEFT of the image (Core Graphics convention). The board and
    pick_face use top-left pixel coordinates, so y is flipped here:
        px = bb.x * W,  py = (1 - bb.y - bb.h) * H,  pw = bb.w * W,  ph = bb.h * H
    W/H are taken from the decoded CGImage, not the wire header, so a JPEG
    whose real size disagrees with w/h still maps correctly.
    """
    import objc
    import Quartz
    import Vision

    with objc.autorelease_pool():
        image = _cgimage_from_frame(frame)
        if image is None:
            raise ValueError(f"could not decode {frame.fmt} frame {frame.seq}")
        width = float(Quartz.CGImageGetWidth(image))
        height = float(Quartz.CGImageGetHeight(image))
        request = Vision.VNDetectFaceRectanglesRequest.alloc().init()
        # nil selects Vision's defaults. A bridged Python dict can raise
        # "key does not exist" during option lookup on current macOS.
        handler = Vision.VNImageRequestHandler.alloc().initWithCGImage_options_(image, None)
        ok, err = handler.performRequests_error_([request], None)
        if not ok:
            raise RuntimeError(f"Vision request failed: {err}")
        rects: list[Rect] = []
        for obs in request.results() or []:
            bb = obs.boundingBox()
            rects.append(Rect(
                x=bb.origin.x * width,
                y=(1.0 - bb.origin.y - bb.size.height) * height,
                w=bb.size.width * width,
                h=bb.size.height * height,
                conf=float(obs.confidence()),
            ))
        return rects


def probe_image_size(data: bytes) -> Optional[tuple[int, int]]:
    """(w, h) of an image file's first frame via ImageIO, or None if unreadable."""
    import Quartz
    from Foundation import NSData

    nsdata = NSData.dataWithBytes_length_(data, len(data))
    source = Quartz.CGImageSourceCreateWithData(nsdata, None)
    if source is None:
        return None
    image = Quartz.CGImageSourceCreateImageAtIndex(source, 0, None)
    if image is None:
        return None
    return int(Quartz.CGImageGetWidth(image)), int(Quartz.CGImageGetHeight(image))


# ---- bench CLI --------------------------------------------------------------

def run_vision_test(path: str) -> int:
    """``eve-bridge vision-test <image>``: detect on a file, print what we'd send."""
    detect = make_detector()
    if detect is None:
        print("vision-test: no detector available on this host", file=sys.stderr)
        return 2
    try:
        data = Path(path).read_bytes()
    except OSError as e:
        print(f"vision-test: cannot read {path}: {e}", file=sys.stderr)
        return 2
    size = probe_image_size(data)
    if size is None:
        print(f"vision-test: {path} is not an image ImageIO can decode", file=sys.stderr)
        return 2
    w, h = size
    # Any ImageIO-decodable file (JPEG, PNG, HEIC...) goes down the "jpeg"
    # branch: that branch is "decode a file with CGImageSource".
    frame = Frame(seq=0, w=w, h=h, fmt="jpeg", data=data)
    detect(frame)                       # warm up: the first Vision call pays model load
    t0 = time.perf_counter()
    rects = detect(frame)
    ms = (time.perf_counter() - t0) * 1000.0
    print(f"{path}: {w}x{h}, {len(rects)} face(s), detect {ms:.1f} ms")
    for r in rects:
        print(f"  rect x={r.x:.0f} y={r.y:.0f} w={r.w:.0f} h={r.h:.0f} conf={r.conf:.2f}  (top-left px)")
    import json
    print("  send:", json.dumps(build_face_cmd(0, pick_face(rects, w, h)), separators=(",", ":")))
    return 0
