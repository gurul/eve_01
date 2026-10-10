"""Idle explorer: when Claude has been quiet for a while, look around and take notes.

The robot spends most of its day watching the user. When no Claude session
has done anything for ``EVE_EXPLORE_AFTER_MIN`` minutes, nothing is
waiting on a card, and the listen key is up, the daemon puts the board in
explore mode and walks its head through a slow pan plan. At each waypoint,
once the head has settled, it keeps one camera frame; if that frame looks
different from the last one it kept at that waypoint (or it has never kept
one there), it spends a note: the frame goes to an OpenAI vision model,
and the one-sentence answer lands in a dated Markdown file.

Wire contract (fixed; the firmware side is built against it):

    host -> board  {"cmd":"look","yaw":<deg -60..60>,"pitch":<deg 5..85>,"hold":<ms>}
    host -> board  {"cmd":"mode","explore":true|false}

Frames keep arriving as ``{"frame":{...}}`` lines (see vision.py); the
explorer only reads them, it never asks for them.

The owner can also send it off by hand: ``eve-bridge explore`` (IPC
``{"evt":"explore","action":"start"}``) or "hey eve, go explore" (the
voice tool ``go_explore``) call ``Explorer.request``, which starts a pan at
once and marks the explore *manual*: the idle timer no longer applies, so
a running Claude session or a hook event does not end it. What does end a
manual explore is a hard sign that the human wants the robot back — the
listen key, a touch on the board, a new wake word, a disconnect — or an
explicit ``Explorer.dismiss`` (``explore stop``).

Two halves, kept apart so the schedule and the budget are testable without
a board, a Mac, or a network (same shape as listen_key.py / vision.py):

* Pure core — ``Explorer`` is a state machine driven by ``tick(...)`` with an
  injected clock. It returns actions (``Mode``, ``Look``, ``Note``) and never
  performs I/O. ``TokenBucket`` is the notes-per-hour budget and
  ``ChangeDetector`` the "did the view change" gate.
* Adapters — ``OpenAINoteClient`` (the network call), ``NoteTaker`` (runs it
  on a thread with a timeout and appends to the notes file), and the
  ``notes`` / ``notes-test`` CLI subcommands.

Budget: one note is at most one request with one low-detail image and
``NOTE_MAX_OUTPUT_TOKENS`` output tokens. The token bucket caps requests at
``EVE_NOTES_PER_HOUR``, and the change detector keeps the bucket from
being spent on a wall that has not moved.
"""

from __future__ import annotations

import asyncio
import base64
import concurrent.futures
import logging
import os
import sys
import time
from collections import deque
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any, Callable, Optional, Protocol, Union

from . import spend
from .vision import Frame, _clamp, encode_gray_png

log = logging.getLogger(__name__)

# ---- configuration -----------------------------------------------------------

DEFAULT_AFTER_MIN = 10.0
DEFAULT_NOTES_PER_HOUR = 6.0
DEFAULT_MODEL = "gpt-6-luna"
DEFAULT_NOTES_DIR = "~/.config/eve-bridge/notes"
DEFAULT_CYCLE_MIN = 15.0

# Pan plan: the whole room the neck can reach. Nine yaws across the servo's
# usable travel at a level gaze, then the same nine coming back while looking
# down at the desk — a serpentine, so no two consecutive waypoints are more
# than 30 degrees apart and the head never has to swing the full span in one
# hold. One ``look`` every LOOK_INTERVAL_SECS with the hold matching it, so
# the board never falls back to its own idle motion between waypoints.
#
# The span is the yaw servo's own: the BSP configures the Feetech SCS0009 at
# id 1 with angleLimit ±128 degrees in position mode, so ±120 is everything
# there is minus a margin. There is no full turn-around to be had — 360
# degrees needs continuous-rotation mode, which gives up position feedback
# and winds the neck loom.
#
# Four pitch rows, not two. The old plan used 40 and 60, both of which turned
# out to point over the desk at the ceiling: the firmware's PITCH_LEVEL of 45
# is only a guess, because level depends on how the head was zeroed (NVS
# ``servo/zero_pos_2``). Bench, 2026-09-06: at pitch 40 eve wrote "I can see
# two recessed lights above", and at pitch 20 it still reported "most of frame
# is ceiling" with a standing person's head at the bottom edge. So the rows
# start at the neck's floor and climb in even steps across the usable band —
# whatever level turns out to be on a given head, some rows are below it.
# An even number of rows also means the serpentine ends where it began, so the
# next cycle opens without a long swing back.
PAN_YAWS: tuple[int, ...] = (-120, -90, -60, -30, 0, 30, 60, 90, 120)
PAN_PITCHES: tuple[int, ...] = (5, 25, 45, 65)
WAYPOINTS: tuple[tuple[int, int], ...] = tuple(
    (yaw, pitch)
    for i, pitch in enumerate(PAN_PITCHES)
    for yaw in (PAN_YAWS if i % 2 == 0 else tuple(reversed(PAN_YAWS)))
)
LOOK_INTERVAL_SECS = 6.0
LOOK_HOLD_MS = 6000
# The head needs a moment to arrive; sample the frame this long after the look.
SETTLE_SECS = 2.0

YAW_RANGE = (-120, 120)
PITCH_RANGE = (5, 85)

# Change detector: mean absolute luma difference (0..255) between 32x24
# thumbnails. 12 is above sensor noise and compression shimmer on a static
# room, below a person walking in or the lights changing.
THUMB_W, THUMB_H = 32, 24
CHANGE_THRESHOLD = 12.0
# Per-waypoint thumbnail bank behind ChangeDetector.surprise: 30 frames is
# about three pan cycles, enough for a median and a spread per cell; below 5
# there is no opinion to give.
BANK_SIZE = 30
BANK_MIN = 5
# Added to every per-cell spread so a perfectly still cell cannot divide by
# zero and turn sensor noise into surprise. 6 luma units is the observed
# frame-to-frame noise of the GC0308 on a static scene.
NOISE_FLOOR = 6.0

NOTE_TIMEOUT_SECS = 20.0
NOTE_MAX_OUTPUT_TOKENS = 1024       # reasoning plus the short visible note
ERROR_LOG_INTERVAL_SECS = 10 * 60.0

PROMPT = (
    "You are a small desk robot looking around a room. In one sentence, note "
    "what you see that a curious pet would remember (objects, people count "
    "without identifying them, light, changes). No preamble."
)


@dataclass(frozen=True)
class ExploreConfig:
    enabled: bool = False          # the idle start; explore itself always works when asked
    after_secs: float = DEFAULT_AFTER_MIN * 60.0
    notes_per_hour: float = DEFAULT_NOTES_PER_HOUR
    model: str = DEFAULT_MODEL
    notes_dir: Path = Path(DEFAULT_NOTES_DIR).expanduser()
    cycle_wait_secs: float = DEFAULT_CYCLE_MIN * 60.0


def _env_float(name: str, default: float, environ: Any) -> float:
    raw = (environ.get(name) or "").strip()
    if not raw:
        return default
    try:
        v = float(raw)
    except ValueError:
        log.warning("explore: %s=%r is not a number; using %g", name, raw, default)
        return default
    if v < 0:
        log.warning("explore: %s=%r is negative; using %g", name, raw, default)
        return default
    return v


def configured(environ: Any = None) -> ExploreConfig:
    """Build the config from EVE_EXPLORE* / EVE_NOTES* env vars."""
    env = os.environ if environ is None else environ
    # Exploring is explicit — "go explore", the CLI, a text — never something eve starts on its own from
    # the idle clock (owner, 2026-09-21). EVE_EXPLORE=1 turns the idle start back on.
    enabled = (env.get("EVE_EXPLORE") or "0").strip().lower() in ("1", "true", "yes", "on")
    notes_dir = (env.get("EVE_NOTES_DIR") or "").strip() or DEFAULT_NOTES_DIR
    return ExploreConfig(
        enabled=enabled,
        after_secs=_env_float("EVE_EXPLORE_AFTER_MIN", DEFAULT_AFTER_MIN, env) * 60.0,
        notes_per_hour=_env_float("EVE_NOTES_PER_HOUR", DEFAULT_NOTES_PER_HOUR, env),
        model=(env.get("EVE_NOTES_MODEL") or "").strip() or DEFAULT_MODEL,
        notes_dir=Path(notes_dir).expanduser(),
        cycle_wait_secs=_env_float("EVE_EXPLORE_CYCLE_MIN", DEFAULT_CYCLE_MIN, env) * 60.0,
    )


# ---- actions (what the daemon executes) ---------------------------------------

@dataclass(frozen=True)
class Mode:
    """Enter (True) or leave (False) explore mode. ``reason`` is for the log."""

    explore: bool
    reason: str


@dataclass(frozen=True)
class Look:
    yaw: int
    pitch: int
    hold_ms: int = LOOK_HOLD_MS


@dataclass(frozen=True)
class Note:
    """Spend one note on this frame, taken at this gaze.

    ``thumb``, ``diff`` and ``surprise`` are what the change detector already
    worked out about the frame and used to discard; the diary's photo gate
    (diary.py) needs them, so they ride along instead of being recomputed.
    """

    frame: Frame
    yaw: int
    pitch: int
    thumb: Optional[bytes] = None      # 32x24 luma, or None when undecodable
    diff: Optional[float] = None       # mean abs luma diff vs the last kept frame here
    surprise: Optional[float] = None   # robust z against this waypoint's history


@dataclass(frozen=True)
class Rest:
    """A pan cycle finished. The board stays in explore mode and looks around
    the room on its own until the next cycle; nothing goes over the wire."""

    reason: str


Action = Union[Mode, Look, Note, Rest]


class ExploreRefused(Exception):
    """``Explorer.request`` cannot start: the reason is the message."""


def build_look_cmd(yaw: float, pitch: float, hold_ms: int = LOOK_HOLD_MS) -> dict[str, Any]:
    return {
        "cmd": "look",
        "yaw": _clamp(yaw, *YAW_RANGE),
        "pitch": _clamp(pitch, *PITCH_RANGE),
        "hold": max(0, int(hold_ms)),
    }


def build_mode_cmd(explore: bool) -> dict[str, Any]:
    return {"cmd": "mode", "explore": bool(explore)}


# ---- budget -------------------------------------------------------------------

class TokenBucket:
    """``per_hour`` tokens refill continuously; the bucket holds at most one
    hour's worth and starts full, so a fresh daemon can note a whole first
    cycle and then settles to the hourly rate."""

    def __init__(self, per_hour: float, now: float, capacity: Optional[float] = None) -> None:
        self.rate = max(0.0, per_hour) / 3600.0
        self.capacity = float(capacity if capacity is not None else max(1.0, per_hour))
        self.tokens = self.capacity if per_hour > 0 else 0.0
        self._at = now

    def _refill(self, now: float) -> None:
        if now > self._at:
            self.tokens = min(self.capacity, self.tokens + (now - self._at) * self.rate)
        self._at = now

    def available(self, now: float) -> float:
        self._refill(now)
        return self.tokens

    def take(self, now: float) -> bool:
        self._refill(now)
        if self.tokens < 1.0:
            return False
        self.tokens -= 1.0
        return True


# ---- change detector ----------------------------------------------------------

def downscale_gray(w: int, h: int, luma: bytes, tw: int = THUMB_W, th: int = THUMB_H) -> bytes:
    """Box-average an 8-bit luma image down to ``tw`` x ``th``. Pure Python;
    runs once per waypoint on a 160x120 frame, so speed does not matter."""
    if len(luma) != w * h:
        raise ValueError(f"luma is {len(luma)} bytes, expected {w * h}")
    if w < tw or h < th:
        raise ValueError(f"cannot downscale {w}x{h} to {tw}x{th}")
    out = bytearray(tw * th)
    for ty in range(th):
        y0 = ty * h // th
        y1 = max(y0 + 1, (ty + 1) * h // th)
        for tx in range(tw):
            x0 = tx * w // tw
            x1 = max(x0 + 1, (tx + 1) * w // tw)
            total = 0
            for y in range(y0, y1):
                row = y * w
                total += sum(luma[row + x0:row + x1])
            out[ty * tw + tx] = total // ((y1 - y0) * (x1 - x0))
    return bytes(out)


def mean_abs_diff(a: bytes, b: bytes) -> float:
    if len(a) != len(b) or not a:
        raise ValueError("thumbnails differ in size")
    return sum(abs(x - y) for x, y in zip(a, b, strict=True)) / len(a)


# A thumbnailer turns a frame into THUMB_W*THUMB_H luma bytes, or None when
# it cannot decode the frame (a JPEG on a host without ImageIO).
Thumbnailer = Callable[[Frame], Optional[bytes]]


def frame_thumb(frame: Frame) -> Optional[bytes]:
    """Default thumbnailer: pure Python for gray frames, ImageIO for JPEG."""
    if frame.fmt == "gray":
        try:
            return downscale_gray(frame.w, frame.h, frame.data)
        except ValueError:
            return None
    return _quartz_thumb(frame.data)


def _quartz_thumb(image: bytes, tw: int = THUMB_W, th: int = THUMB_H) -> Optional[bytes]:
    """Decode any ImageIO-readable file and draw it into a tw x th gray bitmap."""
    if sys.platform != "darwin":
        return None
    try:
        import Quartz
        from Foundation import NSData
    except ImportError:
        return None
    nsdata = NSData.dataWithBytes_length_(image, len(image))
    source = Quartz.CGImageSourceCreateWithData(nsdata, None)
    if source is None:
        return None
    cgimage = Quartz.CGImageSourceCreateImageAtIndex(source, 0, None)
    if cgimage is None:
        return None
    ctx = Quartz.CGBitmapContextCreate(
        None, tw, th, 8, 0, Quartz.CGColorSpaceCreateDeviceGray(), Quartz.kCGImageAlphaNone)
    if ctx is None:
        return None
    Quartz.CGContextSetInterpolationQuality(ctx, Quartz.kCGInterpolationLow)
    Quartz.CGContextDrawImage(ctx, Quartz.CGRectMake(0, 0, tw, th), cgimage)
    thumb = Quartz.CGBitmapContextCreateImage(ctx)
    if thumb is None:
        return None
    stride = Quartz.CGImageGetBytesPerRow(thumb)
    raw = bytes(Quartz.CGDataProviderCopyData(Quartz.CGImageGetDataProvider(thumb)))
    return b"".join(raw[y * stride:y * stride + tw] for y in range(th))


def normalise(thumb: bytes) -> bytes:
    """Centre a thumbnail on mid-grey, so the sun going in or a lamp being
    dimmed does not read as the room changing."""
    mean = sum(thumb) / len(thumb)
    return bytes(min(255, max(0, int(v - mean + 128))) for v in thumb)


def robust_z(thumb: bytes, bank: "deque[bytes]", noise_floor: float = NOISE_FLOOR) -> float:
    """How odd this thumbnail is against what this waypoint usually looks like.

    Per cell: |value - median| / (1.4826 * MAD + noise floor), averaged over
    the 768 cells. Around 1 for a typical view of a familiar spot, 4 and up
    when something real has changed.

    The per-cell MAD is what makes this safe to act on. A cell that always
    flickers — a monitor, a fan, leaves in a window — has a large MAD, so its
    contribution collapses towards zero: the "noisy TV" that defeats plain
    prediction-error curiosity cannot hold eve's attention (Burda et al.
    2018, Random Network Distillation). A small change against a still
    background, on the other hand, scores high, which is the isolation effect
    the memory literature describes (von Restorff).
    """
    cur = normalise(thumb)
    n = len(cur)
    total = 0.0
    for c in range(n):
        col = sorted(b[c] for b in bank)
        mid = len(col) // 2
        median = col[mid]
        mad = sorted(abs(v - median) for v in col)[mid]
        total += abs(cur[c] - median) / (1.4826 * mad + noise_floor)
    return total / n


class ChangeDetector:
    """Per-waypoint memory of the last frame a note was spent on.

    ``measure`` returns the mean absolute difference against that memory, or
    None when there is nothing to compare with (first visit, or a frame the
    host cannot decode) — the caller treats None as "changed". ``keep``
    records a frame as the new reference for its waypoint.

    It also keeps a small bank of recent thumbnails per waypoint, which
    ``surprise`` scores a new one against (see ``robust_z``). The bank holds
    every frame the explorer considered, not only the ones a note was spent
    on, and a frame is scored *before* it is banked, so a view is surprising
    exactly once and then becomes the new normal within a few looks.
    """

    def __init__(self, threshold: float = CHANGE_THRESHOLD, thumb: Thumbnailer = frame_thumb,
                 bank_size: int = BANK_SIZE, bank_min: int = BANK_MIN) -> None:
        self.threshold = threshold
        self.thumb = thumb
        self.bank_size = bank_size
        self.bank_min = bank_min
        self._ref: dict[int, bytes] = {}
        self._bank: dict[int, "deque[bytes]"] = {}

    def measure(self, key: int, frame: Frame) -> Optional[float]:
        ref = self._ref.get(key)
        if ref is None:
            return None
        cur = self.thumb(frame)
        if cur is None or len(cur) != len(ref):
            return None
        return mean_abs_diff(cur, ref)

    def keep(self, key: int, frame: Frame) -> None:
        cur = self.thumb(frame)
        if cur is None:
            self._ref.pop(key, None)
        else:
            self._ref[key] = cur

    def surprise(self, key: int, thumb: Optional[bytes]) -> Optional[float]:
        """Score this thumbnail against the waypoint's bank, then bank it.
        None while the bank is too small to have an opinion."""
        if thumb is None:
            return None
        bank = self._bank.setdefault(key, deque(maxlen=self.bank_size))
        score = robust_z(thumb, bank) if len(bank) >= self.bank_min else None
        bank.append(normalise(thumb))          # read before write: score, then learn
        return score

    def banked(self, key: int) -> int:
        return len(self._bank.get(key, ()))


# ---- the state machine --------------------------------------------------------

class Explorer:
    """Pure schedule. ``tick`` it about once a second with the current facts
    and execute whatever it returns.

    States: OFF (watching the user) -> EXPLORING (walking the pan plan) ->
    RESTING (cycle done, waiting ``cycle_wait_secs`` before the next one).
    The board is in explore mode through both EXPLORING and RESTING: while
    resting the host holds no ``look``, so the firmware looks around the room
    on its own. Any activity — ``idle_secs`` dropping below ``after_secs``,
    the listen key, a disconnect — sends it back to OFF from either state,
    and that is when ``mode explore false`` goes to the board.

    ``request`` (the owner asked) enters EXPLORING from OFF or RESTING at
    once and sets ``manual``: while manual, ``idle_secs`` is ignored, so only
    the listen key, a disconnect or ``dismiss`` ends it. ``manual`` clears
    on every return to OFF.
    """

    OFF = "off"
    EXPLORING = "exploring"
    RESTING = "resting"

    def __init__(
        self,
        config: ExploreConfig,
        now: float = 0.0,
        bucket: Optional[TokenBucket] = None,
        detector: Optional[ChangeDetector] = None,
        waypoints: tuple[tuple[int, int], ...] = WAYPOINTS,
        look_interval: float = LOOK_INTERVAL_SECS,
        settle: float = SETTLE_SECS,
        hold_ms: int = LOOK_HOLD_MS,
        notes_enabled: bool = True,
    ) -> None:
        self.config = config
        self.bucket = bucket if bucket is not None else TokenBucket(config.notes_per_hour, now)
        self.detector = detector if detector is not None else ChangeDetector()
        self.waypoints = waypoints
        self.look_interval = look_interval
        self.settle = settle
        self.hold_ms = hold_ms
        self.notes_enabled = notes_enabled
        self.state = self.OFF
        self.manual = False         # the owner asked; the idle timer does not apply
        self.started_reason = ""    # why the current explore began (for status)
        self.cycles = 0             # completed pan cycles
        self.notes = 0              # Note actions emitted
        self.skipped_same = 0       # frames that matched the reference
        self.skipped_budget = 0     # changed frames the bucket refused
        self._wp = 0
        self._look_at = float("-inf")
        self._sampled = False
        self._rest_until = 0.0

    @property
    def active(self) -> bool:
        return self.state == self.EXPLORING

    @property
    def on_board(self) -> bool:
        """True while the board has been put in explore mode (panning or resting)."""
        return self.state in (self.EXPLORING, self.RESTING)

    @property
    def waypoint(self) -> tuple[int, int]:
        return self.waypoints[self._wp]

    def wants_frame(self, now: float) -> bool:
        """True while a frame for the current waypoint is due and not yet taken."""
        return self.active and not self._sampled and now - self._look_at >= self.settle

    def reset(self) -> None:
        self.state = self.OFF
        self.manual = False
        self._sampled = False

    def _blocker(self, idle_secs: float, listening: bool, connected: bool) -> Optional[str]:
        if not connected:
            return "board disconnected"
        if listening:
            return "listen key"
        if idle_secs < self.config.after_secs and not self.manual:
            return "activity"
        return None

    def status(self) -> dict[str, Any]:
        """A snapshot for ``eve-bridge explore status`` and the log."""
        return {
            "state": self.state,
            "manual": self.manual,
            "reason": self.started_reason,
            "waypoint": list(self.waypoint) if self.active else None,
            "cycles": self.cycles,
            "notes": self.notes,
        }

    def request(
        self,
        now: float,
        reason: str,
        listening: bool = False,
        connected: bool = True,
    ) -> list[Action]:
        """The owner asked for an explore: start a pan now, ignoring idle time.

        From OFF the actions include ``Mode(True)``; from RESTING the board is
        already in explore mode, so only the first ``Look`` goes out; while
        EXPLORING the current pan restarts from its first waypoint (no
        ``Mode``). Raises ``ExploreRefused`` on a hard blocker.
        """
        blocker = self._blocker(float("inf"), listening, connected)
        if blocker is not None:
            raise ExploreRefused(blocker)
        was_on_board = self.on_board
        self.manual = True
        actions = self._start(now, float("inf"), reason=reason)
        if was_on_board:
            return [a for a in actions if not isinstance(a, Mode)]
        return actions

    def dismiss(self, reason: str) -> list[Action]:
        """End an explore (manual or idle) now. ``Mode(False)`` goes out only
        when the board was put in explore mode; ``manual`` always clears."""
        on_board = self.on_board
        self.reset()
        return [Mode(False, reason)] if on_board else []

    def tick(
        self,
        now: float,
        idle_secs: float,
        listening: bool,
        frame: Optional[Frame] = None,
        connected: bool = True,
    ) -> list[Action]:
        blocker = self._blocker(idle_secs, listening, connected)
        if self.state == self.OFF:
            if blocker is None and self.config.enabled:
                return self._start(now, idle_secs)
            return []
        if self.state == self.RESTING:
            if blocker is not None:
                # The board is still in explore mode (looking around on its
                # own); hand the head back to the persona.
                self.reset()
                return [Mode(False, blocker)]
            if now >= self._rest_until:
                # A manual explore keeps its reason across cycles.
                return self._start(now, idle_secs, reason=self.started_reason if self.manual else None)
            return []
        # EXPLORING
        if blocker is not None:
            self.reset()
            return [Mode(False, blocker)]
        actions: list[Action] = []
        if frame is not None and self.wants_frame(now):
            self._sampled = True
            actions.extend(self._consider(now, frame))
        if now - self._look_at >= self.look_interval:
            self._wp += 1
            self._sampled = False
            if self._wp >= len(self.waypoints):
                self.cycles += 1
                self.state = self.RESTING
                self._rest_until = now + self.config.cycle_wait_secs
                actions.append(Rest(
                    f"cycle complete, board looks around on its own; "
                    f"next pan in {self.config.cycle_wait_secs / 60:.0f} min"))
            else:
                self._look_at = now
                yaw, pitch = self.waypoint
                actions.append(Look(yaw, pitch, self.hold_ms))
        return actions

    def _start(self, now: float, idle_secs: float, reason: Optional[str] = None) -> list[Action]:
        self.state = self.EXPLORING
        self._wp = 0
        self._look_at = now
        self._sampled = False
        self.started_reason = reason if reason is not None else f"idle {idle_secs / 60:.0f} min"
        yaw, pitch = self.waypoint
        return [Mode(True, self.started_reason), Look(yaw, pitch, self.hold_ms)]

    def _consider(self, now: float, frame: Frame) -> list[Action]:
        if not self.notes_enabled:
            return []
        # Every considered frame is measured and banked, spent or not: the
        # waypoint's idea of "usual" should include the boring views too.
        thumb = self.detector.thumb(frame)
        diff = self.detector.measure(self._wp, frame)
        surprise = self.detector.surprise(self._wp, thumb)
        if not (diff is None or diff > self.detector.threshold):
            self.skipped_same += 1
            return []
        if not self.bucket.take(now):
            self.skipped_budget += 1
            return []
        self.detector.keep(self._wp, frame)
        self.notes += 1
        yaw, pitch = self.waypoint
        return [Note(frame, yaw, pitch, thumb=thumb, diff=diff, surprise=surprise)]


# ---- note client (the network) ------------------------------------------------

class NoteClient(Protocol):
    def describe(self, image: bytes, mime: str) -> str:
        """Blocking. One sentence about the image. Raises on any failure."""
        ...


def frame_image(frame: Frame) -> tuple[bytes, str]:
    """The bytes to send for a frame and their MIME type."""
    if frame.fmt == "jpeg":
        return frame.data, "image/jpeg"
    return encode_gray_png(frame.w, frame.h, frame.data), "image/png"


def data_url(image: bytes, mime: str) -> str:
    return f"data:{mime};base64,{base64.b64encode(image).decode('ascii')}"


class OpenAINoteClient:
    """Responses API, one ``input_image`` as a base64 data URL at low detail.

    Low detail means a fixed small token cost per image regardless of its
    size; ``max_output_tokens`` caps the answer; ``reasoning.effort=low``
    keeps a reasoning model from spending that cap on thinking. Retries are
    off — a failed note is skipped, not retried into the budget.
    """

    def __init__(self, model: str, api_key: Optional[str] = None, timeout: float = NOTE_TIMEOUT_SECS) -> None:
        from . import openrouter

        self.model = model
        self._client = openrouter.client(api_key, timeout=timeout, max_retries=0)

    def describe(self, image: bytes, mime: str) -> str:
        resp = self._client.responses.create(
            model=self.model,
            input=[{
                "role": "user",
                "content": [
                    {"type": "input_text", "text": PROMPT},
                    {"type": "input_image", "image_url": data_url(image, mime), "detail": "low"},
                ],
            }],
            max_output_tokens=NOTE_MAX_OUTPUT_TOKENS,
            reasoning={"effort": "low"},
        )
        spend.record_response(spend.EXPLORING, resp, model=self.model)
        text = " ".join((resp.output_text or "").split())
        if not text:
            raise RuntimeError(
                f"empty answer (status={resp.status}, incomplete={resp.incomplete_details})")
        return text


def make_note_client(config: ExploreConfig, environ: Any = None) -> Optional[NoteClient]:
    """The real client, or None (with one warning) when notes cannot run."""
    env = os.environ if environ is None else environ
    from . import openrouter

    key = openrouter.key(env)
    if not key:
        log.warning("explore: OPENROUTER_API_KEY not set — the robot will look around but take no notes "
                    "(put it in ~/.config/eve-bridge/env)")
        return None
    try:
        return OpenAINoteClient(config.model, api_key=key)
    except ImportError as e:
        log.warning("explore: openai SDK not importable (%s) — no notes (pip install openai)", e)
        return None


# ---- notes file ---------------------------------------------------------------

def note_line(when: datetime, yaw: int, pitch: int, text: str) -> str:
    return f"- {when:%H:%M} yaw={yaw:+d} pitch={pitch} — {text}"


def notes_path(notes_dir: Path, when: datetime) -> Path:
    return notes_dir / f"{when:%Y-%m-%d}.md"


def append_note(notes_dir: Path, when: datetime, yaw: int, pitch: int, text: str,
                extra: Optional[str] = None) -> Path:
    """Append one line to today's file, and ``extra`` (a Markdown image line
    for a photo) under it. The directory is created private (0700)."""
    if not notes_dir.exists():
        notes_dir.mkdir(parents=True, exist_ok=True)
        try:
            os.chmod(notes_dir, 0o700)
        except OSError:
            pass
    path = notes_path(notes_dir, when)
    with path.open("a", encoding="utf-8") as f:
        f.write(note_line(when, yaw, pitch, text) + "\n")
        if extra:
            f.write(extra + "\n")
    return path


def read_notes(notes_dir: Path, last: int = 20) -> list[str]:
    """The most recent ``last`` note lines across dated files, oldest first.
    ``last=0`` means all. Each line is prefixed with its date."""
    files = sorted(notes_dir.glob("*.md")) if notes_dir.is_dir() else []
    out: list[str] = []
    for path in reversed(files):
        lines = [ln for ln in path.read_text(encoding="utf-8").splitlines() if ln.strip()]
        day = path.stem
        chunk = [f"{day} {ln}" for ln in lines]
        out = chunk + out
        if last and len(out) >= last:
            break
    return out[-last:] if last else out


# ---- async side: run the call with a timeout, write the file ------------------

class NoteTaker:
    """Runs ``NoteClient.describe`` on a worker thread with a timeout and
    appends the sentence to the notes file. Errors are logged at most once
    per ``ERROR_LOG_INTERVAL_SECS`` and the note is skipped."""

    def __init__(
        self,
        client: NoteClient,
        notes_dir: Path,
        timeout: float = NOTE_TIMEOUT_SECS,
        clock: Callable[[], float] = time.monotonic,
        wall: Callable[[], datetime] = datetime.now,
    ) -> None:
        self.client = client
        self.notes_dir = notes_dir
        self.timeout = timeout
        self.clock = clock
        self.wall = wall
        self.taken = 0
        self.failed = 0
        self._err_logged_at = float("-inf")
        self._executor: Optional[concurrent.futures.ThreadPoolExecutor] = None

    def stop(self) -> None:
        if self._executor is not None:
            self._executor.shutdown(wait=False, cancel_futures=True)
            self._executor = None

    async def take(self, note: Note) -> Optional[Path]:
        image, mime = frame_image(note.frame)
        loop = asyncio.get_running_loop()
        if self._executor is None:
            self._executor = concurrent.futures.ThreadPoolExecutor(
                max_workers=1, thread_name_prefix="notes")
        try:
            text = await asyncio.wait_for(
                loop.run_in_executor(self._executor, self.client.describe, image, mime),
                timeout=self.timeout,
            )
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001 — any failure is "skip this note"
            self.failed += 1
            self._log_error(e)
            return None
        path = append_note(self.notes_dir, self.wall(), note.yaw, note.pitch, text)
        self.taken += 1
        log.info("explore: note -> %s", path)
        return path

    def _log_error(self, e: BaseException) -> None:
        now = self.clock()
        if now - self._err_logged_at < ERROR_LOG_INTERVAL_SECS:
            return
        self._err_logged_at = now
        what = "timed out" if isinstance(e, asyncio.TimeoutError) else f"{type(e).__name__}: {e}"
        log.warning("explore: note failed — %s (further failures muted for %d min)",
                    what, int(ERROR_LOG_INTERVAL_SECS // 60))


# ---- CLI -----------------------------------------------------------------------

def run_notes_cli(last: int, config: Optional[ExploreConfig] = None) -> int:
    """``eve-bridge notes [--last N]``: print recent notes."""
    cfg = config if config is not None else configured()
    lines = read_notes(cfg.notes_dir, last)
    if not lines:
        print(f"no notes yet in {cfg.notes_dir}")
        return 0
    for ln in lines:
        print(ln)
    return 0


def run_notes_test(path: str, config: Optional[ExploreConfig] = None) -> int:
    """``eve-bridge notes-test <image>``: one real call, print the sentence."""
    cfg = config if config is not None else configured()
    try:
        image = Path(path).read_bytes()
    except OSError as e:
        print(f"notes-test: cannot read {path}: {e}", file=sys.stderr)
        return 2
    mime = "image/png" if path.lower().endswith(".png") else "image/jpeg"
    client = make_note_client(cfg)
    if client is None:
        print("notes-test: no note client (OPENROUTER_API_KEY missing or openai not installed)", file=sys.stderr)
        return 2
    t0 = time.perf_counter()
    try:
        text = client.describe(image, mime)
    except Exception as e:  # noqa: BLE001
        print(f"notes-test: {type(e).__name__}: {e}", file=sys.stderr)
        return 1
    ms = (time.perf_counter() - t0) * 1000.0
    print(f"{cfg.model} ({ms:.0f} ms): {text}")
    return 0
