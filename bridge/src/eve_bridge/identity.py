"""Owner identity: tag each detected face ``owner`` or ``unknown``.

The robot should know whose desk it sits on. While the listen key is held
(Option = dictation, so the owner is at the laptop, facing the robot) the
daemon enrols the face it sees; afterwards every face cmd carries
``who:"owner"`` when the print of the largest face is close to one of the
enrolled prints, ``who:"unknown"`` otherwise.

Two halves, kept apart so the policy and the store are testable without a
Mac (same shape as vision.py and listen_key.py):

* Pure core — ``OwnerIdentity`` holds up to ``CAPACITY`` owner prints as
  plain lists of floats, classifies by minimum distance against them,
  persists to JSON, and owns the enrolment policy (``EnrolmentGate``).
  ``FaceIdentity`` is the per-frame driver: print, classify, maybe enrol,
  log transitions. Everything it touches is injectable.
* Platform adapter, darwin — ``make_describer`` wraps Vision's
  ``VNGenerateImageFeaturePrintRequest`` (revision 2) over a CGImage
  cropped to the face rect padded by 25 %.

Storage choice: Vision's ``computeDistance:toFeaturePrintObservation:`` is
plain Euclidean distance over the observation's raw float32 ``data()``
(measured identical to 1e-8 on macOS 26.5, revision 1 and 2). So the
adapter unpacks the print into a Python list of floats once and the core
does the arithmetic itself. No observation objects are kept and nothing is
rebuilt on load; the JSON file holds the vectors verbatim.

Revision 2 (macOS 14+) prints are 768 unit-norm floats, so distances lie in
0..2. ``DEFAULT_THRESHOLD`` is a starting point, not a measurement — tune it
on the bench with ``EVE_OWNER_THRESHOLD`` and the ``identity: ... d=``
log lines.

Thread model: ``FaceIdentity.process`` runs on vision.py's single-worker
executor, right after the detect for the same frame, so Vision requests
never overlap. The core takes a lock around every read and write, because
``reset`` (IPC) and ``status`` come from the asyncio loop thread.
"""

from __future__ import annotations

import json
import logging
import math
import os
import sys
import threading
import time
from pathlib import Path
from typing import Any, Callable, Optional

from .vision import Frame, Rect, largest_rect

log = logging.getLogger(__name__)

WHO_OWNER = "owner"
WHO_UNKNOWN = "unknown"

# How many owner prints we keep. More prints = more poses/lighting covered.
CAPACITY = 24
# Min distance to any enrolled print below which a face is the owner.
DEFAULT_THRESHOLD = 0.9
# Enrolment policy: only with the listen key down, exactly one face in the
# frame, the face at least this wide (as % of frame width), and at most one
# enrolment per interval so a single 3 s hold does not fill the store with
# near-identical prints.
MIN_FACE_SIZE_PCT = 20
ENROL_INTERVAL_SECS = 2.0
# Transition log lines ("owner recognised" / "unknown face") at most this often.
TRANSITION_LOG_SECS = 30.0
# Feature print revision we request (VNGenerateImageFeaturePrintRequestRevision2).
FEATURE_PRINT_REVISION = 2
# Grow the face rect by this fraction of its size on every side before printing.
CROP_PAD = 0.25
# Persisted file format version.
FILE_VERSION = 1

Vector = list[float]
Distance = Callable[[Vector, Vector], float]
# Executor thread. Feature print of one face; None when Vision could not
# produce one (bad crop, decode failure).
Describer = Callable[[Frame, Rect], Optional[Vector]]


def default_path() -> Path:
    override = os.environ.get("EVE_OWNER_PRINTS")
    if override:
        return Path(override).expanduser()
    base = os.environ.get("XDG_CONFIG_HOME")
    root = Path(base) if base else Path.home() / ".config"
    return root / "eve-bridge" / "owner_faceprints.json"


def configured_threshold() -> float:
    raw = (os.environ.get("EVE_OWNER_THRESHOLD") or "").strip()
    if not raw:
        return DEFAULT_THRESHOLD
    try:
        value = float(raw)
    except ValueError:
        log.warning("identity: bad EVE_OWNER_THRESHOLD=%r; using %.2f", raw, DEFAULT_THRESHOLD)
        return DEFAULT_THRESHOLD
    if not math.isfinite(value) or value <= 0:
        log.warning("identity: EVE_OWNER_THRESHOLD=%r out of range; using %.2f", raw, DEFAULT_THRESHOLD)
        return DEFAULT_THRESHOLD
    return value


def euclidean(a: Vector, b: Vector) -> float:
    """What Vision's computeDistance does over the raw print floats."""
    if len(a) != len(b):
        raise ValueError(f"print length mismatch: {len(a)} vs {len(b)}")
    return math.sqrt(sum((x - y) * (x - y) for x, y in zip(a, b, strict=True)))


# ---- pure core --------------------------------------------------------------

class OwnerIdentity:
    """The owner's prints, classification, and the JSON store. Thread-safe."""

    def __init__(
        self,
        distance: Distance = euclidean,
        threshold: float = DEFAULT_THRESHOLD,
        capacity: int = CAPACITY,
        path: Optional[Path] = None,
    ) -> None:
        self.distance = distance
        self.threshold = threshold
        self.capacity = capacity
        self.path = path
        self.prints: list[Vector] = []
        self.enrolled_at: Optional[float] = None    # wall clock of the last enrolment
        self._lock = threading.Lock()

    def __len__(self) -> int:
        with self._lock:
            return len(self.prints)

    def classify(self, vec: Vector) -> tuple[str, Optional[float]]:
        """("owner"|"unknown", min distance). Distance is None with no prints."""
        with self._lock:
            prints = list(self.prints)
        if not prints:
            return WHO_UNKNOWN, None
        d = min(self.distance(vec, p) for p in prints)
        return (WHO_OWNER if d < self.threshold else WHO_UNKNOWN), d

    def enrol(self, vec: Vector, now: Optional[float] = None) -> int:
        """Add a print. When full, replace the enrolled print nearest to the new
        one, so the store keeps its spread over poses instead of drifting to
        one look. Returns the 1-based slot the print went into."""
        with self._lock:
            if len(self.prints) < self.capacity:
                self.prints.append(list(vec))
                slot = len(self.prints)
            else:
                idx = min(range(len(self.prints)), key=lambda i: self.distance(vec, self.prints[i]))
                self.prints[idx] = list(vec)
                slot = idx + 1
            self.enrolled_at = time.time() if now is None else now
            return slot

    def reset(self) -> int:
        """Forget every print, in memory and on disk. Returns how many were held."""
        with self._lock:
            n = len(self.prints)
            self.prints = []
            self.enrolled_at = None
            path = self.path
        if path is not None:
            try:
                path.unlink()
            except FileNotFoundError:
                pass
        return n

    # ---- persistence ----

    def snapshot(self) -> dict[str, Any]:
        with self._lock:
            return {
                "version": FILE_VERSION,
                "revision": FEATURE_PRINT_REVISION,
                "threshold": self.threshold,
                "enrolled_at": self.enrolled_at,
                "prints": [list(p) for p in self.prints],
            }

    def save(self, path: Optional[Path] = None) -> Path:
        """Write the store as JSON, mode 600, via a temp file + rename."""
        target = path if path is not None else self.path
        if target is None:
            raise ValueError("no path to save to")
        target.parent.mkdir(parents=True, exist_ok=True)
        tmp = target.with_name(target.name + ".tmp")
        fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            json.dump(self.snapshot(), f, separators=(",", ":"))
        os.chmod(tmp, 0o600)
        os.replace(tmp, target)
        return target

    def load(self, path: Optional[Path] = None) -> int:
        """Read the store. Missing file = empty store. A malformed file is
        logged and ignored (never raises). Returns the number of prints."""
        source = path if path is not None else self.path
        if source is None:
            return 0
        try:
            raw = json.loads(source.read_text(encoding="utf-8"))
            prints = _parse_prints(raw)
        except FileNotFoundError:
            return 0
        except (OSError, ValueError, TypeError) as e:
            log.warning("identity: ignoring %s: %s", source, e)
            return 0
        enrolled_at = raw.get("enrolled_at")
        with self._lock:
            self.prints = prints[: self.capacity]
            self.enrolled_at = float(enrolled_at) if isinstance(enrolled_at, (int, float)) else None
            return len(self.prints)


def _parse_prints(raw: Any) -> list[Vector]:
    if not isinstance(raw, dict):
        raise ValueError("not a JSON object")
    if raw.get("version") != FILE_VERSION:
        raise ValueError(f"unsupported version {raw.get('version')!r}")
    if raw.get("revision") != FEATURE_PRINT_REVISION:
        raise ValueError(f"prints are revision {raw.get('revision')!r}, want {FEATURE_PRINT_REVISION} — reset")
    prints = raw.get("prints")
    if not isinstance(prints, list):
        raise ValueError("prints is not a list")
    out: list[Vector] = []
    for p in prints:
        if not isinstance(p, list) or not p:
            raise ValueError("print is not a non-empty list")
        vec = [float(x) for x in p]
        if any(isinstance(x, bool) or not math.isfinite(x) for x in vec):
            raise ValueError("print holds a non-finite value")
        out.append(vec)
    if len({len(p) for p in out}) > 1:
        raise ValueError("prints differ in length")
    return out


class EnrolmentGate:
    """When may this face be enrolled? Listen key down, exactly one face,
    face wide enough, and the interval since the last enrolment elapsed."""

    def __init__(
        self,
        min_size_pct: int = MIN_FACE_SIZE_PCT,
        interval: float = ENROL_INTERVAL_SECS,
    ) -> None:
        self.min_size_pct = min_size_pct
        self.interval = interval
        self._last = float("-inf")

    def allows(self, listen_down: bool, n_faces: int, size_pct: float, now: float) -> bool:
        if not listen_down or n_faces != 1 or size_pct < self.min_size_pct:
            return False
        return now - self._last >= self.interval

    def mark(self, now: float) -> None:
        self._last = now


class FaceIdentity:
    """Per-frame driver. ``process`` runs on the vision executor thread.

    ``listen_down`` is polled per frame (the daemon owns the key state);
    ``describe`` turns the face crop into a print, or None.
    """

    def __init__(
        self,
        core: OwnerIdentity,
        describe: Describer,
        listen_down: Callable[[], bool] = lambda: False,
        gate: Optional[EnrolmentGate] = None,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        self.core = core
        self.describe = describe
        self.listen_down = listen_down
        self.gate = gate if gate is not None else EnrolmentGate()
        self.clock = clock
        self.enrolments = 0
        self.last_who: Optional[str] = None     # last classified face
        self.last_distance: Optional[float] = None
        self._transition_logged_at = float("-inf")
        self._save_failed = False

    def process(self, frame: Frame, rects: list[Rect]) -> str:
        """Executor thread. Classify the largest face, enrol per policy. Never raises."""
        if not rects:
            return WHO_UNKNOWN
        face = largest_rect(rects)
        try:
            vec = self.describe(frame, face)
        except Exception:  # noqa: BLE001
            log.exception("identity: feature print failed on frame %d", frame.seq)
            return WHO_UNKNOWN
        if vec is None:
            return WHO_UNKNOWN
        now = self.clock()
        size_pct = face.w / frame.w * 100.0 if frame.w > 0 else 0.0
        if self.gate.allows(self.listen_down(), len(rects), size_pct, now):
            self.gate.mark(now)
            slot = self.core.enrol(vec)
            self.enrolments += 1
            log.info("identity: enrolled owner print #%d (size=%.0f%%, %d/%d held)",
                     slot, size_pct, len(self.core), self.core.capacity)
            self._persist()
        who, d = self.core.classify(vec)
        self._note(who, d, now)
        return who

    def _persist(self) -> None:
        if self.core.path is None:
            return
        try:
            self.core.save()
            self._save_failed = False
        except OSError as e:
            if not self._save_failed:
                log.warning("identity: could not save %s: %s", self.core.path, e)
            self._save_failed = True

    def _note(self, who: str, d: Optional[float], now: float) -> None:
        self.last_distance = d
        if who == self.last_who:
            return
        self.last_who = who
        if now - self._transition_logged_at < TRANSITION_LOG_SECS:
            return
        self._transition_logged_at = now
        dist = f" (d={d:.2f})" if d is not None else ""
        if who == WHO_OWNER:
            log.info("identity: owner recognised%s", dist)
        else:
            log.info("identity: unknown face%s", dist)

    def status(self) -> dict[str, Any]:
        return {
            "prints": len(self.core),
            "capacity": self.core.capacity,
            "threshold": self.core.threshold,
            "path": str(self.core.path) if self.core.path is not None else None,
            "enrolled_at": self.core.enrolled_at,
            "last_who": self.last_who,
            "last_distance": self.last_distance,
            "enrolments": self.enrolments,
        }


def crop_rect(face: Rect, width: float, height: float, pad: float = CROP_PAD) -> tuple[int, int, int, int]:
    """Face rect grown by ``pad`` of its size on each side, clamped to the image.
    Returns integer (x, y, w, h) in top-left pixel coordinates."""
    x0 = max(0.0, face.x - face.w * pad)
    y0 = max(0.0, face.y - face.h * pad)
    x1 = min(width, face.x + face.w * (1.0 + pad))
    y1 = min(height, face.y + face.h * (1.0 + pad))
    x, y = int(math.floor(x0)), int(math.floor(y0))
    w, h = int(math.ceil(x1)) - x, int(math.ceil(y1)) - y
    return x, y, max(0, w), max(0, h)


# ---- platform adapter: macOS Vision ------------------------------------------

def make_describer() -> Optional[Describer]:
    """The Vision feature-print describer, or None (with one warning) when unavailable."""
    if sys.platform != "darwin":
        log.warning("identity: owner recognition needs macOS Vision — every face stays 'unknown'")
        return None
    try:
        import Quartz  # noqa: F401
        import Vision
    except ImportError as e:
        log.warning("identity: pyobjc Vision/Quartz not importable (%s) — every face stays 'unknown'", e)
        return None
    if not hasattr(Vision, "VNGenerateImageFeaturePrintRequestRevision2"):
        log.warning("identity: Vision feature print revision 2 needs macOS 14+ — every face stays 'unknown'")
        return None
    return vision_feature_print


def vision_feature_print(frame: Frame, face: Rect) -> Optional[Vector]:
    """Executor thread. VNGenerateImageFeaturePrintRequest (revision 2) on the
    padded face crop. The crop uses CGImageCreateWithImageInRect, whose rect
    is in top-left pixel coordinates — the same space as Rect — so no flip."""
    import struct

    import objc
    import Quartz
    import Vision

    from .vision import _cgimage_from_frame

    with objc.autorelease_pool():
        image = _cgimage_from_frame(frame)
        if image is None:
            return None
        width = float(Quartz.CGImageGetWidth(image))
        height = float(Quartz.CGImageGetHeight(image))
        x, y, w, h = crop_rect(face, width, height)
        if w < 2 or h < 2:
            return None
        crop = Quartz.CGImageCreateWithImageInRect(image, Quartz.CGRectMake(x, y, w, h))
        if crop is None:
            return None
        request = Vision.VNGenerateImageFeaturePrintRequest.alloc().init()
        request.setRevision_(Vision.VNGenerateImageFeaturePrintRequestRevision2)
        # Keep options native/nil; the Python dict bridge can throw on an
        # absent Vision option instead of returning nil.
        handler = Vision.VNImageRequestHandler.alloc().initWithCGImage_options_(crop, None)
        ok, err = handler.performRequests_error_([request], None)
        if not ok:
            raise RuntimeError(f"Vision feature print failed: {err}")
        results = request.results() or []
        if not results:
            return None
        obs = results[0]
        count = int(obs.elementCount())
        data = bytes(obs.data())
        if len(data) != count * 4:
            raise RuntimeError(f"feature print is {len(data)} bytes for {count} elements; expected float32")
        return list(struct.unpack(f"<{count}f", data))


# ---- CLI ---------------------------------------------------------------------

def run_identity(action: str, socket_path: Optional[str] = None) -> int:
    """``eve-bridge identity status|reset``.

    Talks to the daemon over IPC when it is running (it holds the prints in
    memory); otherwise works on the file directly.
    """
    from .hooks._client import post

    path = default_path()
    resp = post({"evt": "identity", "action": action}, socket_path=socket_path, timeout=3.0)
    if action == "reset":
        if resp is not None:
            print(f"owner prints reset ({resp.get('removed', 0)} removed) — {path}")
            return 0
        core = OwnerIdentity(path=path)
        n = core.load()
        core.reset()
        print(f"daemon not reachable; removed {path} ({n} prints). A running daemon keeps its "
              "in-memory prints until restart.")
        return 0
    if resp is not None and isinstance(resp.get("identity"), dict):
        _print_status(resp["identity"], live=True, enabled=bool(resp.get("enabled")))
        return 0
    core = OwnerIdentity(path=path, threshold=configured_threshold())
    core.load()
    _print_status(FaceIdentity(core, describe=lambda f, r: None).status(), live=False, enabled=None)
    return 0


def _print_status(st: dict[str, Any], live: bool, enabled: Optional[bool]) -> None:
    src = "daemon" if live else "file (daemon not reachable)"
    print(f"owner prints: {st.get('prints', 0)}/{st.get('capacity', CAPACITY)}  [{src}]")
    print(f"file:         {st.get('path')}")
    print(f"threshold:    {st.get('threshold')}  (EVE_OWNER_THRESHOLD)")
    at = st.get("enrolled_at")
    if isinstance(at, (int, float)):
        print(f"last enrol:   {time.strftime('%Y-%m-%d %H:%M:%S', time.localtime(at))}")
    if live:
        print(f"recognition:  {'on' if enabled else 'off (no Vision on this host)'}")
        who, d = st.get("last_who"), st.get("last_distance")
        if who is not None:
            print(f"last face:    {who}" + (f" (d={d:.2f})" if isinstance(d, (int, float)) else ""))
        print(f"enrolments:   {st.get('enrolments', 0)} this run")
    if not st.get("prints"):
        print("\nto enrol: hold Option while facing the robot for a few seconds")
