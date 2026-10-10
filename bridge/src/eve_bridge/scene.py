"""Scene: what eve can see, as short timestamped sentences for the voice.

The voice model (gpt-live-1) takes audio and text only — images are an
unsupported modality on its model page, and the Live API in the SDK has no
image input. So a cheap image model looks for it: while a conversation is
open, the newest camera frame goes to ``gpt-6-luna`` at most once every
``interval_secs``, and what it says comes back as one line such as

    [vision 16:43:05] A person at a desk holding a green mug. Change: they picked up the mug.

Nothing here is pushed to the voice. Each line goes into the watcher's own
journal (``notes``) and the newest view waits in ``latest``; the voice reads
it only when the owner asks, through the backend's ``look`` tool. (Pushing
every line as silent context made the voice narrate the room unprompted,
owner report 2026-09-10.) head.py's ``look_around`` / ``find`` use
``look(newer_than=...)`` and ``locate`` so a view is always from a frame taken
after the head stopped turning.

Privacy, by construction:

* Nothing runs outside a conversation. ``offer`` ignores frames until
  ``start``, and ``stop`` drops the frame slot and every observation.
* One frame at most is held, in memory, and it is released as soon as it is
  handed to the describer. Nothing is ever written to disk here.
* The API call sets ``store=False``, so the response is not kept for the
  dashboard or for later retrieval.

Honesty about time: every line carries the wall-clock time the frame was
*seen*, not the time the answer came back. When no frame arrives for
``camera_lost_secs`` (board gone, camera off), the journal records once that
the camera is lost and ``look`` answers that nothing can be seen. When the
view is unchanged, a "no change since" line — stamped with the newest view
that confirmed it — is journaled every ``refresh_secs``.

Two halves, as in vision.py: a pure core (``SceneWatcher`` with an injected
client and clock, ``parse_scene``, ``format_note``) that runs in tests, and
``OpenAISceneClient``, the only part that touches the network.
"""

from __future__ import annotations

import asyncio
import concurrent.futures
import json
import logging
import os
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timedelta
from pathlib import Path
from typing import Any, Callable, Optional, Protocol

from . import spend
from .explore import data_url, frame_image
from .vision import decode_frame

log = logging.getLogger(__name__)

DEFAULT_MODEL = "gpt-6-luna"
DEFAULT_INTERVAL_SECS = 3.0      # minimum gap between two describes
DEFAULT_STALE_SECS = 15.0        # a view older than this is "out of date"
DEFAULT_CAMERA_LOST_SECS = 3.0   # no frame for this long: the camera is gone
DEFAULT_TIMEOUT_SECS = 8.0       # one describe; slower answers are dropped
DEFAULT_REFRESH_SECS = 20.0      # repeat "no change" this often
MAX_OUTPUT_TOKENS = 2048     # includes reasoning; the schema/prompt still bounds the visible answer
NOTE_MAX_CHARS = 300             # session.thinking.append allows 500 tokens; stay far below
LOOK_FRESH_SECS = 4.0            # the look tool reuses a view this young
TICK_SECS = 0.25
FRAME_WAIT_POLL_SECS = 0.05

PROMPT = (
    "You are the eyes of a small desk robot. This is one frame from its camera. "
    "In `scene`, describe what is in view in one plain sentence of at most 25 words: "
    "people (what they are doing, what they hold), notable objects, and the setting. "
    "Say only what you can see. If the frame is dark, blurry or blocked, say that. "
    "Never guess names or identities. "
    "In `change`, say in at most 12 words what differs from the previous description, "
    "or an empty string if nothing meaningful changed."
)

SCHEMA: dict[str, Any] = {
    "type": "object",
    "properties": {
        "scene": {"type": "string"},
        "change": {"type": "string"},
    },
    "required": ["scene", "change"],
    "additionalProperties": False,
}

LOCATE_PROMPT = (
    "You are the eyes of a small desk robot looking for something. This is one frame from its camera. "
    "Is the thing described below visible in this frame? If it is, give the centre of it: "
    "`x` from -100 (left edge of the image) to 100 (right edge), `y` from -100 (top) to 100 (bottom), "
    "and in `what` name what you found in a few words. If it is not visible, set `visible` to false, "
    "`x` and `y` to 0 and `what` to an empty string. Do not guess.\nLooking for: "
)

LOCATE_SCHEMA: dict[str, Any] = {
    "type": "object",
    "properties": {
        "visible": {"type": "boolean"},
        "x": {"type": "number"},
        "y": {"type": "number"},
        "what": {"type": "string"},
    },
    "required": ["visible", "x", "y", "what"],
    "additionalProperties": False,
}


@dataclass(frozen=True)
class SceneConfig:
    enabled: bool = True
    model: str = DEFAULT_MODEL
    interval_secs: float = DEFAULT_INTERVAL_SECS
    stale_secs: float = DEFAULT_STALE_SECS
    camera_lost_secs: float = DEFAULT_CAMERA_LOST_SECS
    timeout_secs: float = DEFAULT_TIMEOUT_SECS
    refresh_secs: float = DEFAULT_REFRESH_SECS


def _float_env(env: Any, name: str, default: float, lo: float, hi: float) -> float:
    raw = (env.get(name) or "").strip()
    if not raw:
        return default
    try:
        return min(hi, max(lo, float(raw)))
    except ValueError:
        log.warning("scene: %s=%r is not a number; using %s", name, raw, default)
        return default


def configured(environ: Any = None) -> SceneConfig:
    """``EVE_SCENE=0`` turns it off; the rest tune it."""
    env = os.environ if environ is None else environ
    enabled = (env.get("EVE_SCENE") or "1").strip().lower() not in ("0", "false", "no", "off")
    model = (env.get("EVE_SCENE_MODEL") or "").strip() or DEFAULT_MODEL
    return SceneConfig(
        enabled=enabled,
        model=model,
        interval_secs=_float_env(env, "EVE_SCENE_INTERVAL_SECS", DEFAULT_INTERVAL_SECS, 1.0, 60.0),
        stale_secs=_float_env(env, "EVE_SCENE_STALE_SECS", DEFAULT_STALE_SECS, 3.0, 300.0),
    )


@dataclass(frozen=True)
class Observation:
    seen_at: float          # monotonic time the frame arrived
    seen_wall: datetime     # the same instant on the wall clock
    text: str
    change: str             # "" when nothing meaningful changed
    latency_secs: float     # how long the describe took
    yaw: Optional[float] = None     # head pose the board echoed on that frame
    pitch: Optional[float] = None


class SceneClient(Protocol):
    def describe(self, image: bytes, mime: str, previous: Optional[str]) -> tuple[str, str]:
        """Blocking. (scene sentence, change phrase or ""). Raises on any failure."""
        ...

    def locate(self, image: bytes, mime: str, target: str) -> dict[str, Any]:
        """Blocking. {"visible", "x", "y", "what"}. Raises on any failure."""
        ...


def parse_scene(raw: str) -> tuple[str, str]:
    """The model's JSON answer → (scene, change). A plain-text answer is the scene."""
    raw = (raw or "").strip()
    if not raw:
        raise ValueError("empty answer")
    try:
        obj = json.loads(raw)
    except ValueError:
        return " ".join(raw.split()), ""
    if not isinstance(obj, dict):
        return " ".join(raw.split()), ""
    scene = " ".join(str(obj.get("scene") or "").split())
    change = " ".join(str(obj.get("change") or "").split())
    if not scene:
        raise ValueError("answer has no scene")
    if change.lower().rstrip(".") in ("none", "no change", "nothing", "n/a", "nothing changed"):
        change = ""
    return scene, change


def parse_locate(raw: str) -> dict[str, Any]:
    obj = json.loads(raw)
    visible = obj.get("visible") is True

    def coord(v: Any) -> float:
        try:
            return max(-100.0, min(100.0, float(v)))
        except (TypeError, ValueError):
            return 0.0

    return {"visible": visible, "x": coord(obj.get("x")) if visible else 0.0,
            "y": coord(obj.get("y")) if visible else 0.0,
            "what": " ".join(str(obj.get("what") or "").split()) if visible else ""}


def _hms(t: datetime) -> str:
    return t.strftime("%H:%M:%S")


def _clip(text: str, n: int = NOTE_MAX_CHARS) -> str:
    return text if len(text) <= n else text[: n - 1].rstrip() + "…"


def _num(v: Any) -> Optional[float]:
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        return None
    return float(v)


def format_note(obs: Observation) -> str:
    """One observation as the line the voice reads."""
    line = f"[vision {_hms(obs.seen_wall)}] {obs.text}"
    if obs.change:
        line += f" Change: {obs.change}"
    return _clip(line)


def unchanged_note(latest: Observation, since: Observation) -> str:
    """Stamped with the newest view that confirmed it — never with 'now', which
    would claim a look that did not happen."""
    return _clip(f"[vision {_hms(latest.seen_wall)}] No change since {_hms(since.seen_wall)}: {latest.text}")


def lost_note(now_wall: datetime, last: Optional[Observation]) -> str:
    base = f"[vision {_hms(now_wall)}] Camera lost: you cannot see anything right now."
    if last is not None:
        base += f" Your last view, from {_hms(last.seen_wall)}, may be out of date."
    return _clip(base)


def blind_note(now_wall: datetime, last: Optional[Observation]) -> str:
    base = f"[vision {_hms(now_wall)}] You cannot make out the view right now."
    if last is not None:
        base += f" Your last clear view is from {_hms(last.seen_wall)} and may be out of date."
    return _clip(base)


NO_CAMERA = "the camera is not connected, so nothing can be seen right now"


class SceneWatcher:
    """Frames in, timestamped notes out. Loop thread only; the describe runs
    on a one-worker executor so the event loop never blocks on the network.

    ``client`` may be set after construction (the daemon resolves it in
    run()). ``camera_ok`` says whether a camera could be streaming at all —
    the board is connected and the host asked it to stream. The watcher
    never calls out: ``look`` and ``locate`` are the only way a view leaves it.
    """

    def __init__(
        self,
        client: Optional[SceneClient],
        config: Optional[SceneConfig] = None,
        clock: Callable[[], float] = time.monotonic,
        wall: Callable[[], datetime] = datetime.now,
        camera_ok: Callable[[], bool] = lambda: True,
        tick_secs: float = TICK_SECS,
    ) -> None:
        self.client = client
        self.config = config or SceneConfig()
        self.clock = clock
        self.wall = wall
        self.camera_ok = camera_ok
        self.tick_secs = tick_secs
        self.active = False
        self.latest: Optional[Observation] = None
        self.notes: list[str] = []      # this conversation's notes, for tests and the log; cleared on stop
        self.describes = 0
        self.failures = 0
        self._frame: Optional[dict[str, Any]] = None
        self._frame_at = float("-inf")
        self._last_describe_at = float("-inf")
        self._last_note_at = float("-inf")
        self._camera_state = "unknown"   # "ok" | "lost" | "unknown"
        self._blind_said = False
        self._force = False
        self._min_frame_at = float("-inf")   # the next describe must use a frame newer than this
        self._busy = False
        self._started_at = float("-inf")
        self._view_since: Optional[Observation] = None   # when the current view last changed
        self._confirmed_at = float("-inf")               # seen_at of the last view a note reported
        self._task: Optional[asyncio.Task[None]] = None
        self._executor: Optional[concurrent.futures.ThreadPoolExecutor] = None

    @property
    def enabled(self) -> bool:
        return self.client is not None and self.config.enabled

    # -- lifecycle --------------------------------------------------------------

    def start(self, run_loop: bool = True) -> None:
        """A conversation opened: begin watching. No-op when disabled.
        ``run_loop=False`` leaves the stepping to the caller (tests call tick())."""
        if not self.enabled or self.active:
            return
        self.active = True
        self._reset()
        self._started_at = self.clock()
        self._executor = concurrent.futures.ThreadPoolExecutor(max_workers=1, thread_name_prefix="scene")
        if run_loop:
            self._task = asyncio.ensure_future(self._loop())

    async def stop(self) -> None:
        """The conversation closed: stop, and forget every frame and observation."""
        self.active = False
        task, self._task = self._task, None
        if task is not None:
            task.cancel()
            await asyncio.gather(task, return_exceptions=True)
        if self._executor is not None:
            self._executor.shutdown(wait=False, cancel_futures=True)
            self._executor = None
        if self.describes:
            log.info("scene: %d view(s) described, %d failed", self.describes, self.failures)
        self._reset()

    def _reset(self) -> None:
        self._frame = None
        self._frame_at = float("-inf")
        self.latest = None
        self.notes = []
        self.describes = 0
        self.failures = 0
        self._last_describe_at = float("-inf")
        self._last_note_at = float("-inf")
        self._camera_state = "unknown"
        self._blind_said = False
        self._force = False
        self._min_frame_at = float("-inf")
        self._view_since = None
        self._confirmed_at = float("-inf")

    def offer(self, raw: dict[str, Any]) -> None:
        """A camera frame off the wire. Kept only while active, and only the newest."""
        if not self.active:
            return
        self._frame = raw
        self._frame_at = self.clock()

    def newest_frame(self) -> Optional[dict[str, Any]]:
        """The newest raw frame, for a photo when the board cannot snap one. None
        when there is none, or it is older than the camera-lost window."""
        if self._frame is None or self.clock() - self._frame_at > self.config.camera_lost_secs:
            return None
        return self._frame

    # -- state ------------------------------------------------------------------

    def camera_state(self, now: float) -> str:
        if not self.camera_ok() or now - self._frame_at > self.config.camera_lost_secs:
            return "lost"
        return "ok"

    def _wall_at(self, t: float) -> datetime:
        return self.wall() - timedelta(seconds=max(0.0, self.clock() - t))

    def _emit(self, note: str) -> None:
        """Journal one line. It goes nowhere else: the voice pulls views with ``look``."""
        self.notes.append(note)
        self._last_note_at = self.clock()
        # The log gets the tag and the kind of line, never what the camera saw.
        kind = ("camera lost" if "Camera lost" in note else "cannot see" if "cannot make out" in note
                else "no change" if "No change since" in note else "view")
        log.info("scene: %s %s", note.split("]", 1)[0] + "]", kind)

    def _usable_frame(self) -> bool:
        return self._frame is not None and self._frame_at > self._min_frame_at

    # -- the loop -----------------------------------------------------------------

    async def _loop(self) -> None:
        while self.active:
            await self.tick()
            await asyncio.sleep(self.tick_secs)

    async def tick(self) -> None:
        """One step: notice camera loss, maybe describe, maybe repeat 'no change'."""
        now = self.clock()
        state = self.camera_state(now)
        # The camera gets camera_lost_secs from start() to deliver a first frame
        # before "camera lost" is said: the board may be starting its stream.
        grace = self._camera_state == "unknown" and now - self._started_at < self.config.camera_lost_secs
        if state == "lost":
            if self._camera_state != "lost" and not grace:
                self._camera_state = "lost"
                self._emit(lost_note(self.wall(), self.latest))
            return
        if self._camera_state == "lost":
            log.info("scene: camera back")
        self._camera_state = "ok"
        due = self._force or now - self._last_describe_at >= self.config.interval_secs
        if due and not self._busy and self._usable_frame():
            await self._describe()
            return
        latest, since = self.latest, self._view_since
        if (latest is not None and since is not None and not self._busy
                and latest.seen_at > self._confirmed_at
                and now - self._last_note_at >= self.config.refresh_secs):
            self._confirmed_at = latest.seen_at
            self._emit(unchanged_note(latest, since))

    async def _describe(self) -> None:
        raw, seen_at = self._frame, self._frame_at
        self._frame = None                      # released: the describer holds the only copy now
        self._force = False
        self._min_frame_at = float("-inf")
        self._busy = True
        self._last_describe_at = self.clock()
        prev = self.latest.text if self.latest is not None else None
        pose_yaw = _num((raw or {}).get("yaw"))
        pose_pitch = _num((raw or {}).get("pitch"))
        try:
            frame = decode_frame(raw or {})
            image, mime = frame_image(frame)
            del raw, frame
            assert self.client is not None and self._executor is not None
            loop = asyncio.get_running_loop()
            t0 = self.clock()
            text, change = await asyncio.wait_for(
                loop.run_in_executor(self._executor, self.client.describe, image, mime, prev),
                timeout=self.config.timeout_secs)
            del image
            latency = self.clock() - t0
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001
            self.failures += 1
            if self.failures == 1 or self.failures % 10 == 0:
                log.warning("scene: describe failed (%d so far): %s: %s", self.failures, type(e).__name__, e)
            self._busy = False
            self._maybe_blind()
            return
        self._busy = False
        self.describes += 1
        self._blind_said = False
        obs = Observation(seen_at=seen_at, seen_wall=self._wall_at(seen_at), text=text,
                          change=change if prev is not None else "", latency_secs=latency,
                          yaw=pose_yaw, pitch=pose_pitch)
        first = self.latest is None
        self.latest = obs
        if first or obs.change:
            self._view_since = obs          # the view as it has been since this moment
            self._confirmed_at = obs.seen_at
            self._emit(format_note(obs))
        if latency > self.config.interval_secs:
            log.info("scene: describe took %.1f s (interval %.1f s)", latency, self.config.interval_secs)

    def _maybe_blind(self) -> None:
        """Describes keep failing and the last view has aged out: say so, once."""
        now = self.clock()
        last = self.latest
        if self._blind_said:
            return
        if last is None or now - last.seen_at > self.config.stale_secs:
            self._blind_said = True
            self._emit(blind_note(self.wall(), last))

    # -- on demand ------------------------------------------------------------------

    def _unavailable(self) -> Optional[dict[str, Any]]:
        if not self.enabled:
            return {"ok": False, "reason": "vision is off on this computer (no API key or EVE_SCENE=0)"}
        if not self.active:
            return {"ok": False, "reason": "vision only runs during a conversation"}
        return None

    async def look(self, fresh_secs: float = LOOK_FRESH_SECS,
                   newer_than: Optional[float] = None) -> dict[str, Any]:
        """The current view, refreshed if it is older than ``fresh_secs`` — or, with
        ``newer_than``, from a frame that arrived after that moment (the head has just
        turned). Never waits longer than one describe timeout."""
        off = self._unavailable()
        if off is not None:
            return off
        if self.camera_state(self.clock()) == "lost":
            return self._answer(ok=False, reason=NO_CAMERA)
        floor = float("-inf") if newer_than is None else newer_than

        def good() -> bool:
            last = self.latest
            if last is None or last.seen_at <= floor:
                return False
            return newer_than is not None or self.clock() - last.seen_at <= fresh_secs

        if good():
            return self._answer(ok=True)
        # Ask the loop for a describe now (of a frame newer than `floor`) and wait
        # until that attempt has finished, one way or the other. The wait is bounded
        # on the event loop's own clock, never the injectable one.
        attempts = self.describes + self.failures
        self._force = True
        self._min_frame_at = floor
        loop = asyncio.get_running_loop()
        deadline = loop.time() + self.config.timeout_secs + self.config.camera_lost_secs
        while loop.time() < deadline and not good():
            if self.describes + self.failures > attempts and not self._force and not self._busy:
                break                                   # the forced attempt is done; it failed or was too old
            if self._camera_state == "lost":
                break
            await asyncio.sleep(FRAME_WAIT_POLL_SECS)
        if self.camera_state(self.clock()) == "lost":
            return self._answer(ok=False, reason=NO_CAMERA)
        if good():
            return self._answer(ok=True)
        if self.describes + self.failures > attempts:
            return self._answer(ok=False, reason="could not make out the view")
        return self._answer(ok=False, reason="no new view came in time")

    async def locate(self, target: str, newer_than: Optional[float] = None) -> dict[str, Any]:
        """Is ``target`` in view? Uses the newest frame (one taken after ``newer_than``
        when given). {"ok", "visible", "x", "y", "what", "yaw", "pitch"}."""
        off = self._unavailable()
        if off is not None:
            return off
        floor = float("-inf") if newer_than is None else newer_than
        loop = asyncio.get_running_loop()
        deadline = loop.time() + self.config.camera_lost_secs
        while not (self._frame is not None and self._frame_at > floor):
            if not self.camera_ok():
                return {"ok": False, "reason": NO_CAMERA}
            if loop.time() >= deadline:
                if self.camera_state(self.clock()) == "lost":
                    return {"ok": False, "reason": NO_CAMERA}
                return {"ok": False, "reason": "no new frame came in time"}
            await asyncio.sleep(FRAME_WAIT_POLL_SECS)
        raw = self._frame or {}
        pose_yaw, pose_pitch = _num(raw.get("yaw")), _num(raw.get("pitch"))
        try:
            frame = decode_frame(raw)
            image, mime = frame_image(frame)
            del raw, frame
            assert self.client is not None and self._executor is not None
            loop = asyncio.get_running_loop()
            found = await asyncio.wait_for(
                loop.run_in_executor(self._executor, self.client.locate, image, mime, target),
                timeout=self.config.timeout_secs)
            del image
        except asyncio.CancelledError:
            raise
        except Exception as e:  # noqa: BLE001
            log.warning("scene: locate %r failed: %s: %s", target, type(e).__name__, e)
            return {"ok": False, "reason": "could not check the view"}
        out: dict[str, Any] = {"ok": True, **found, "yaw": pose_yaw, "pitch": pose_pitch}
        log.info("scene: locate %r → %s", target,
                 f"visible at x={found.get('x'):.0f} y={found.get('y'):.0f}" if found.get("visible") else "not visible")
        return out

    def _answer(self, ok: bool, reason: Optional[str] = None) -> dict[str, Any]:
        now = self.clock()
        out: dict[str, Any] = {"ok": ok}
        last = self.latest
        if last is not None:
            age = now - last.seen_at
            out.update({"view": last.text, "seen_at": _hms(last.seen_wall), "age_secs": round(age, 1),
                        "stale": age > self.config.stale_secs})
        if reason is not None:
            out["reason"] = reason
        elif last is None:
            out["reason"] = "no clear view yet"
        return out


# ---- the network ------------------------------------------------------------------

class OpenAISceneClient:
    """Responses API: one low-detail ``input_image``, a strict JSON answer, and
    ``store=False`` so nothing is retained on OpenAI's side for later retrieval.
    Retries off: a slow or failed view is skipped, the next frame comes soon."""

    def __init__(self, model: str, api_key: Optional[str] = None,
                 timeout: float = DEFAULT_TIMEOUT_SECS) -> None:
        from . import openrouter

        self.model = model
        self._client = openrouter.client(api_key, timeout=timeout, max_retries=0)

    def _body(self, prompt: str, image: bytes, mime: str, name: str, schema: dict[str, Any]) -> dict[str, Any]:
        return {
            "model": self.model,
            "input": [{
                "role": "user",
                "content": [
                    {"type": "input_text", "text": prompt},
                    {"type": "input_image", "image_url": data_url(image, mime), "detail": "low"},
                ],
            }],
            "text": {"format": {"type": "json_schema", "name": name, "strict": True, "schema": schema}},
            "max_output_tokens": MAX_OUTPUT_TOKENS,
            "reasoning": {"effort": "low"},
            "store": False,
        }

    def request(self, image: bytes, mime: str, previous: Optional[str]) -> dict[str, Any]:
        """The exact describe request body (tests check store=False and the image part)."""
        prompt = PROMPT + (f"\nPrevious description: {previous}" if previous else
                           "\nThere is no previous description; set change to an empty string.")
        return self._body(prompt, image, mime, "scene", SCHEMA)

    def locate_request(self, image: bytes, mime: str, target: str) -> dict[str, Any]:
        return self._body(LOCATE_PROMPT + " ".join(target.split())[:80], image, mime, "locate", LOCATE_SCHEMA)

    def describe(self, image: bytes, mime: str, previous: Optional[str]) -> tuple[str, str]:
        resp = self._client.responses.create(**self.request(image, mime, previous))
        spend.record_response(spend.CAMERA, resp, model=self.model)
        text = resp.output_text or ""
        if not text.strip():
            raise RuntimeError(f"empty answer (status={resp.status}, incomplete={resp.incomplete_details})")
        return parse_scene(text)

    def locate(self, image: bytes, mime: str, target: str) -> dict[str, Any]:
        resp = self._client.responses.create(**self.locate_request(image, mime, target))
        spend.record_response(spend.CAMERA, resp, model=self.model)
        text = resp.output_text or ""
        if not text.strip():
            raise RuntimeError(f"empty answer (status={resp.status}, incomplete={resp.incomplete_details})")
        return parse_locate(text)


def make_scene_client(config: SceneConfig, environ: Any = None) -> Optional[SceneClient]:
    """The real client, or None (with one log line) when vision cannot run."""
    env = os.environ if environ is None else environ
    if not config.enabled:
        log.info("scene: off (EVE_SCENE=0) — eve cannot describe what it sees")
        return None
    from . import openrouter

    key = openrouter.key(env)
    if not key:
        log.warning("scene: OPENROUTER_API_KEY not set — eve cannot describe what it sees")
        return None
    try:
        client = OpenAISceneClient(config.model, api_key=key, timeout=config.timeout_secs)
    except ImportError as e:
        log.warning("scene: openai SDK not importable (%s) — no scene descriptions", e)
        return None
    log.info("scene: describing camera views with %s during conversations (every %.0f s at most)",
             config.model, config.interval_secs)
    return client


# ---- bench CLI ----------------------------------------------------------------------

def _mime_for(data: bytes) -> Optional[str]:
    if data[:2] == b"\xff\xd8":
        return "image/jpeg"
    if data[:8] == b"\x89PNG\r\n\x1a\n":
        return "image/png"
    return None


def run_scene_test(path: str, find: Optional[str] = None) -> int:
    """``eve-bridge scene-test <image> [--find thing]``: one real describe (and
    locate), timed. Prints ``scene-test ok (<model>, <ms> ms)`` on success."""
    cfg = configured()
    client = make_scene_client(SceneConfig(model=cfg.model, timeout_secs=cfg.timeout_secs))
    if client is None:
        print("scene-test: no client (OPENROUTER_API_KEY not set?)", file=sys.stderr)
        return 2
    try:
        data = Path(path).read_bytes()
    except OSError as e:
        print(f"scene-test: cannot read {path}: {e}", file=sys.stderr)
        return 2
    mime = _mime_for(data)
    if mime is None:
        print(f"scene-test: {path} is not a JPEG or PNG", file=sys.stderr)
        return 2
    t0 = time.perf_counter()
    try:
        scene, change = client.describe(data, mime, None)
    except Exception as e:  # noqa: BLE001
        print(f"scene-test: describe failed: {type(e).__name__}: {e}", file=sys.stderr)
        return 1
    ms = (time.perf_counter() - t0) * 1000.0
    if ms > cfg.timeout_secs * 1000.0:
        print(f"scene-test: answer took {ms:.0f} ms, over the {cfg.timeout_secs:.0f} s timeout", file=sys.stderr)
        return 1
    print(f"scene: {scene}")
    print(f"change: {change or '(none)'}")
    if find:
        t1 = time.perf_counter()
        try:
            loc = client.locate(data, mime, find)
        except Exception as e:  # noqa: BLE001
            print(f"scene-test: locate failed: {type(e).__name__}: {e}", file=sys.stderr)
            return 1
        print(f"locate {find!r}: {loc} ({(time.perf_counter() - t1) * 1000.0:.0f} ms)")
    print(f"scene-test ok ({cfg.model}, {ms:.0f} ms)")
    return 0
