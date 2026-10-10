"""The hands: a persistent PyAutoGUI REPL that runs the model's Python on this Mac.

Runs as a child process of the daemon (`python -m eve_bridge.desktop_worker`),
one JSON request per stdin line, one JSON reply per stdout line:

    {"id": 1, "operation": "execute", "code": "display(pyautogui.screenshot())"}
    {"id": 1, "output": [{"type":"input_text","text":...}, {"type":"input_image",...}]}

Globals persist between calls, exactly like the `exec_py` tool in OpenAI's
computer-use sample app (openai/openai-cua-sample-app, python-app/app/desktop/
worker.py, MIT) which this file follows: `log(...)` collects text, `display(img)`
collects PNGs, exceptions come back as text so the model can correct itself,
and the PyAutoGUI fail-safe (mouse into a screen corner) is a terminal error.

Retina: PyAutoGUI screenshots are physical pixels (2x) while its input is
logical points. `normalize_screenshots` resizes every screenshot to the
point grid so the coordinates the model reads are the coordinates it clicks.

On top of raw PyAutoGUI the namespace carries the helpers from
desktop_helpers.py — open_app, open_url, frontmost, screen_text, find_text,
click_text, wait_for, wait_settled, type_text, zoom, observe — which sense
the screen locally (CG capture, Vision OCR, AX) and log one sentence each.
With EVE_FAST_LANE on (default: fast_lane.FAST_LANE_DEFAULT) it also
carries `delegate`; in the lane's `model` mode `start_fast_lane` loads the hosted
decider (jev.py) in a daemon thread. The ready line and every reply say where
that stands ("fast_lane").

Auto-screenshot rule: when a call clicked, typed or pressed keys (any
PyAutoGUI input function, type_text, click_text), raised, or produced no
output, and did not display() an image itself, `execute` appends a screenshot
taken after the screen settles (≤ 1.5 s) — then always an `[after]` line with
the frontmost app and whether the screen changed since the previous call.
Text-only calls (screen_text, find_text) stay cheap: no image.

`{"operation": "observe"}` is `execute("observe()")`: the first turn's picture.
With helpers, every reply also carries `"timing": {…}` in ms (capture, resize,
encode, ocr, ax, settle, act, decide, exec) so the agent's run log shows where
local time goes.
`python -m eve_bridge.desktop_worker --release` posts a key-up for every
key code and a mouse-up for every button, in a fresh process, so the daemon
can release anything a killed worker left held.

Nothing here is imported by the daemon; the daemon talks to it over the
pipe (computer_agent.py). `execute()` and `normalize_screenshots()` are plain
functions so tests can drive them with a fake pyautogui.
"""

from __future__ import annotations

import base64
import contextlib
import io
import json
import os
import signal
import sys
import threading
import time
import traceback
from typing import Any

MAX_TEXT_BYTES = 60 * 1024
MAX_IMAGE_BYTES = 8 * 1024 * 1024
MAX_ITEMS = 250
MAX_CODE_BYTES = 64 * 1024
MAX_REQUEST_BYTES = 512 * 1024


def emit(payload: dict[str, Any]) -> None:
    out = sys.__stdout__
    if out is None:
        raise RuntimeError("worker stdout is unavailable")
    out.write(json.dumps(payload) + "\n")
    out.flush()


def check_desktop(pyautogui: Any) -> tuple[int, int]:
    """Fail loudly at startup when the daemon cannot see or touch the screen."""
    width, height = pyautogui.size()
    if width <= 0 or height <= 0:
        raise RuntimeError("no desktop: the daemon must run in the user's GUI session (LaunchAgent)")
    if sys.platform == "darwin":
        import ctypes

        services = ctypes.CDLL("/System/Library/Frameworks/ApplicationServices.framework/ApplicationServices")
        services.AXIsProcessTrusted.restype = ctypes.c_bool
        if not services.AXIsProcessTrusted():
            raise RuntimeError(
                f"Accessibility is not granted to {os.path.realpath(sys.executable)} — "
                "System Settings > Privacy & Security > Accessibility")
        import Quartz

        if not Quartz.CGPreflightScreenCaptureAccess():
            raise RuntimeError(
                f"Screen Recording is not granted to {os.path.realpath(sys.executable)} — "
                "System Settings > Privacy & Security > Screen & System Audio Recording")
    return int(width), int(height)


def normalize_screenshots(pyautogui: Any) -> None:
    """Make pyautogui.screenshot() return images in point coordinates (Retina 2x → 1x)."""
    from PIL import Image

    capture = pyautogui.screenshot

    def screenshot(imageFilename: Any = None, region: Any = None, **kwargs: Any) -> Any:
        image = capture(**kwargs)
        size = tuple(pyautogui.size())
        if tuple(image.size) != size:
            image = image.resize(size, Image.Resampling.LANCZOS)
        if region is not None:
            x, y, w, h = region
            image = image.crop((x, y, x + w, y + h))
        if imageFilename is not None:
            image.save(imageFilename)
        return image

    pyautogui.screenshot = screenshot


def execute(code: str, namespace: dict[str, Any], helpers: Any = None) -> dict[str, Any]:
    """Run one exec_py call. Returns {"output": [...]} or {"error": {...}} (fail-safe).

    With `helpers` (desktop_helpers.Helpers) the auto-screenshot rule from the
    module docstring applies and an `[after]` line closes every reply.
    """
    from PIL import Image

    output: list[dict[str, Any]] = []
    text_bytes = 0
    image_bytes = 0
    t_start = time.perf_counter()
    encode_secs = 0.0

    def log(*values: Any) -> None:
        nonlocal text_bytes
        text = " ".join(str(v) for v in values)
        text_bytes += len(text.encode("utf-8"))
        if text_bytes > MAX_TEXT_BYTES or len(output) >= MAX_ITEMS:
            raise ValueError("text output exceeds its size limit")
        output.append({"type": "input_text", "text": text})

    def display(value: Any) -> None:
        nonlocal image_bytes, encode_secs
        if isinstance(value, Image.Image):
            t0 = time.perf_counter()
            buf = io.BytesIO()
            value.save(buf, format="PNG")
            value = buf.getvalue()
            encode_secs += time.perf_counter() - t0
        if not isinstance(value, bytes) or not value.startswith(b"\x89PNG\r\n\x1a\n"):
            raise TypeError("display() expects a Pillow image or PNG bytes")
        image_bytes += len(value)
        if image_bytes > MAX_IMAGE_BYTES or len(output) >= MAX_ITEMS:
            raise ValueError("image output exceeds its size limit")
        output.append({"type": "input_image", "detail": "original",
                       "image_url": "data:image/png;base64," + base64.b64encode(value).decode("ascii")})

    class Writer:
        def write(self, text: str) -> int:
            if text.strip():
                log(text.rstrip("\n"))
            return len(text)

        def flush(self) -> None:
            pass

    namespace.update(log=log, display=display)
    if helpers is not None:
        helpers.bind(log, display)
        helpers.begin()
    errored = False
    try:
        with contextlib.redirect_stdout(Writer()), contextlib.redirect_stderr(Writer()):
            exec(compile(code, "<exec_py>", "exec"), namespace)  # noqa: S102 — the model's code, on purpose
    except BaseException as error:  # noqa: BLE001 — every error goes back to the model
        failsafe = getattr(namespace.get("pyautogui"), "FailSafeException", ())
        if failsafe and isinstance(error, failsafe):
            return {"error": {"code": "failsafe", "message": "desktop fail-safe: the mouse hit a screen corner"}}
        errored = True
        output.append({"type": "input_text", "text": traceback.format_exc()[-4000:]})
    if helpers is not None:
        # An action needs its settled result in the same reply (a blind repeat
        # or a screenshot-only turn costs 3-5 s of model time; 1.5 s here caps
        # the worker at ~2 s). Text-only calls stay cheap; failed or empty
        # calls still end with a picture so the next turn can act.
        has_image = any(o["type"] == "input_image" for o in output)
        if not has_image and (helpers.acted or errored or not output):
            with contextlib.suppress(Exception):
                display(helpers.settled_screenshot(1.5 if helpers.acted else 0.0))
        with contextlib.suppress(Exception):
            log(helpers.after_line())
    result: dict[str, Any] = {"output": output or [{"type": "input_text", "text": "exec_py completed with no output."}]}
    if helpers is not None:
        timing = dict(helpers.timing_ms())
        timing["encode"] = round(encode_secs * 1000, 2)
        timing["exec"] = round((time.perf_counter() - t_start) * 1000, 2)
        result["timing"] = timing
        if getattr(helpers, "fast_lane", False):
            result["fast_lane"] = str(helpers.fast_lane_status)
    return result


def fast_lane_config(env: Any) -> tuple[bool, str]:
    """(enabled, style) from EVE_FAST_LANE / EVE_FAST_LANE_STYLE.
    The default for the switch is fast_lane.FAST_LANE_DEFAULT, which the holdout eval sets."""
    from .decider import STYLES
    from .fast_lane import DEFAULT_STYLE, FAST_LANE_DEFAULT

    raw = (env.get("EVE_FAST_LANE") or "").strip().lower()
    enabled = FAST_LANE_DEFAULT if not raw else raw not in ("0", "false", "no", "off")
    style = (env.get("EVE_FAST_LANE_STYLE") or "").strip().lower()
    if style not in STYLES:
        style = DEFAULT_STYLE                  # "jev": the style Jev scored best with on select (jev.py)
    return enabled, style


def lane_modes(env: Any) -> tuple[str, bool]:
    """(who decides a step, whether the router may run) from EVE_FAST_LANE_DECIDE and
    EVE_LANE_FIRST. The defaults are fast_lane.DEFAULT_DECIDE and fast_lane.LANE_FIRST_DEFAULT."""
    from .fast_lane import DECIDE_MODES, DEFAULT_DECIDE, LANE_FIRST_DEFAULT

    decide = (env.get("EVE_FAST_LANE_DECIDE") or "").strip().lower()
    if decide not in DECIDE_MODES:
        decide = DEFAULT_DECIDE
    raw = (env.get("EVE_LANE_FIRST") or "").strip().lower()
    first = LANE_FIRST_DEFAULT if not raw else raw not in ("0", "false", "no", "off")
    return decide, first


DECIDER_BACKENDS = ("jev",)


def decider_backend(env: Any) -> str:
    """Who answers the lane's one `choice` question in `model` mode: EVE_DECIDER, unset by default.

    "jev" is TypeSafe's hosted System One model over decider.Decider's predict seam (jev.py): a
    network call, and the state it sends carries the focused window's title and visible text, so
    it runs only when the owner names it. Unset or any other value is "" — no decider, and
    `model` mode stays off rather than sending anything anywhere.
    """
    name = (env.get("EVE_DECIDER") or "").strip().lower()
    return name if name in DECIDER_BACKENDS else ""


def start_fast_lane(helpers: Any, env: Any, loader: Any = None, thread: bool = True) -> str:
    """Turn the lane on for `helpers`, and in `model` mode load the decider off the critical path.

    Returns the status the ready line carries: "ready (keyword gate)" (no model is needed),
    "loading" (a daemon thread is running `loader(style)`, jev.load by default), or
    "off (<reason>)". Later replies carry `helpers.fast_lane_status`: "ready (load … warm …)"
    or "failed: <one line>" — the delegate helper answers `unavailable` until ready.
    """
    enabled, style = fast_lane_config(env)
    decide, first = lane_modes(env)
    backend = decider_backend(env)
    helpers.lane_decide = decide
    helpers.lane_first_on = bool(first) and sys.platform == "darwin"
    router = "; router on" if helpers.lane_first_on else ""
    if not enabled:
        helpers.fast_lane_status = "off (EVE_FAST_LANE)" + router
        return helpers.fast_lane_status
    if sys.platform != "darwin":
        helpers.fast_lane_status = f"off (not macOS: {sys.platform})"
        return helpers.fast_lane_status
    if decide == "keyword" or (decide == "jev" and not backend):
        # The keyword gate is code, and in "jev" mode the step asker (start_jev_asker) decides: neither
        # needs a decider, so the lane is ready now and nothing is loaded.
        helpers.fast_lane = True
        helpers.fast_lane_status = ("ready (keyword gate)" if decide == "keyword"
                                    else "ready (jev step asker; no decider)") + router
        return helpers.fast_lane_status
    if not backend:
        helpers.fast_lane_status = "off (model mode needs EVE_DECIDER=jev)" + router
        return helpers.fast_lane_status
    helpers.fast_lane = True
    helpers.fast_lane_status = "loading"

    def load() -> None:
        try:
            if loader is not None:
                decider = loader(style)
            else:
                from . import jev

                decider = jev.load(env=env, style=style)
            helpers.decider = decider
            loaded = (f"load {getattr(decider, 'load_ms', 0):.0f} ms, "
                      f"warm {getattr(decider, 'warm_ms', 0):.0f} ms, style {style}")
            helpers.fast_lane_status = f"ready ({loaded})" + router
        except Exception as e:  # noqa: BLE001 — the lane stays unavailable; the model keeps its ordinary helpers
            helpers.fast_lane_status = f"failed: {type(e).__name__}: {e}"[:200]

    if thread:
        threading.Thread(target=load, name="fast-lane-loader", daemon=True).start()
    else:
        load()
    return helpers.fast_lane_status


def start_jev_asker(helpers: Any, env: Any) -> str:
    """Give `helpers` a Jev step asker when the owner's switches call for one: the lane's "jev" decide mode
    (EVE_FAST_LANE_DECIDE=jev) or the plan executor (EVE_PLAN_EXEC=1). Returns a status for the
    ready line. No request is sent here — the first step pays its own 240 ms — and a route with no key
    leaves the asker unset, so the keyword gate decides alone rather than the lane going dark."""
    decide, _first = lane_modes(env)
    plan_exec = (env.get("EVE_PLAN_EXEC") or "").strip().lower() in ("1", "true", "yes", "on")
    if decide != "jev" and not plan_exec:
        return "off"
    import time

    from . import jev
    from .typed_ask import make_step_asker

    try:
        url, key, model = jev.route_config(env)
    except jev.JevError as e:
        return f"off ({e})"[:160]
    seconds = jev.timeout_from_env(env)
    helpers.step_asker = make_step_asker(jev.make_predict(url, key, model, timeout_s=seconds), time.perf_counter, env)
    return f"ready ({model})"


def outline(req: dict[str, Any], helpers: Any) -> dict[str, Any]:
    """The `outline` operation: the front window's controls, as the planner is shown them."""
    if helpers is None:
        return {"outline": {"app": "", "lines": [], "error": "no helpers in this worker"}}
    try:
        return {"outline": dict(helpers.outline())}
    except Exception as e:  # noqa: BLE001 — no outline means the planner plans blind, never a protocol error
        return {"outline": {"app": "", "lines": [], "error": f"{type(e).__name__}: {e}"[:200]}}


def run_plan(req: dict[str, Any], helpers: Any) -> dict[str, Any]:
    """The `run_plan` operation: walk a plan with no planner turn between its steps (plan_executor.py)."""
    plan = req.get("plan")
    if not isinstance(plan, dict) or helpers is None:
        return {"run_plan": {"status": "unavailable", "reason": "run_plan needs a plan object"}}
    lines: list[str] = []
    helpers.bind(lambda *values: lines.append(" ".join(str(v) for v in values)), lambda _image: None)
    helpers.begin()
    try:
        result = dict(helpers.run_plan(plan, str(req.get("request") or ""), start=int(req.get("start") or 0),
                                       approved=req.get("approved") if isinstance(req.get("approved"), dict) else None,
                                       dry_run=bool(req.get("dry_run"))))
    except Exception as e:  # noqa: BLE001
        result = {"status": "partial", "next_index": int(req.get("start") or 0), "ledger": [],
                  "reason": f"error:{type(e).__name__}: {e}"[:200]}
    result.setdefault("log", lines)
    return {"run_plan": result, "timing": dict(helpers.timing_ms())}


def lane_first(req: dict[str, Any], helpers: Any) -> dict[str, Any]:
    """The `lane_first` operation: the router's attempt at the goal before any planner call
    (lane_router.RouteResult.to_dict(), plus the worker's timing). Never a terminal error: a
    worker without the router answers {"status": "unavailable"} and the planner runs as before."""
    goal = req.get("goal")
    if not isinstance(goal, str) or not goal.strip():
        return {"lane_first": {"status": "unavailable", "reason": "lane_first needs a string goal"}}
    if helpers is None or not getattr(helpers, "lane_first_on", False):
        return {"lane_first": {"status": "unavailable", "reason": "the router is off (EVE_LANE_FIRST)"}}
    lines: list[str] = []
    helpers.bind(lambda *values: lines.append(" ".join(str(v) for v in values)), lambda _image: None)
    helpers.begin()
    try:
        result = dict(helpers.lane_first(goal, dry_run=bool(req.get("dry_run"))))
    except Exception as e:  # noqa: BLE001 — the router failing is "the planner does it"
        result = {"status": "none", "reason": f"error:{type(e).__name__}"}
    result.setdefault("log", lines)
    return {"lane_first": result, "timing": dict(helpers.timing_ms())}


def serve(namespace: dict[str, Any], stream: Any, helpers: Any = None) -> None:
    terminal: Any = None
    for line in stream:
        if len(line.encode("utf-8")) > MAX_REQUEST_BYTES:
            emit({"error": {"code": "request_too_large", "message": "request exceeds 512 KiB"}})
            continue
        try:
            req = json.loads(line)
        except ValueError:
            emit({"error": {"code": "bad_json", "message": "request is not JSON"}})
            continue
        rid = req.get("id") if isinstance(req, dict) else None
        if terminal is not None:
            emit({"id": rid, "error": terminal})
            continue
        if isinstance(req, dict) and req.get("operation") == "observe":
            result = execute("observe()", namespace, helpers)
            terminal = result.get("error")
            emit({"id": rid, **result})
            continue
        if isinstance(req, dict) and req.get("operation") == "lane_first":
            emit({"id": rid, **lane_first(req, helpers)})
            continue
        if isinstance(req, dict) and req.get("operation") == "outline":
            emit({"id": rid, **outline(req, helpers)})
            continue
        if isinstance(req, dict) and req.get("operation") == "run_plan":
            emit({"id": rid, **run_plan(req, helpers)})
            continue
        if not isinstance(req, dict) or req.get("operation") != "execute":
            emit({"id": rid, "error": {"code": "unsupported", "message": "only execute, observe, "
                                       "lane_first, outline and run_plan are supported"}})
            continue
        code = req.get("code")
        if not isinstance(code, str) or not code.strip() or len(code.encode("utf-8")) > MAX_CODE_BYTES:
            emit({"id": rid, "error": {"code": "bad_code", "message": "code must be a non-empty string under 64 KiB"}})
            continue
        result = execute(code, namespace, helpers)
        terminal = result.get("error")
        emit({"id": rid, **result})


def release_inputs(quartz: Any) -> int:
    """Post a key-up for every key code (0..127) and a mouse-up for each button.

    A killed worker can leave a modifier or a mouse button held at the HID
    level; posting the ups clears that. Port of openai/openai-cua-sample-app
    python-app/app/desktop/release_inputs.py (MIT). Returns the events posted.
    """
    count = 0
    for code in range(128):
        quartz.CGEventPost(quartz.kCGHIDEventTap, quartz.CGEventCreateKeyboardEvent(None, code, False))
        count += 1
    location = quartz.CGEventGetLocation(quartz.CGEventCreate(None))
    buttons = ((quartz.kCGEventLeftMouseUp, quartz.kCGMouseButtonLeft),
               (quartz.kCGEventRightMouseUp, quartz.kCGMouseButtonRight),
               (quartz.kCGEventOtherMouseUp, quartz.kCGMouseButtonCenter))
    for kind, button in buttons:
        quartz.CGEventPost(quartz.kCGHIDEventTap, quartz.CGEventCreateMouseEvent(None, kind, location, button))
        count += 1
    return count


def main() -> None:
    import pyautogui

    from .desktop_helpers import Helpers

    width, height = check_desktop(pyautogui)
    normalize_screenshots(pyautogui)
    pyautogui.FAILSAFE = True
    pyautogui.PAUSE = 0.05
    # __builtins__ stays open on purpose: exec_py is unrestricted Python as the
    # daemon user (docs/stackchan/voice.md, Safety); the helpers exist so the
    # model has no reason to reach past them.
    namespace: dict[str, Any] = {"__builtins__": __builtins__, "pyautogui": pyautogui, "time": time}
    helpers = Helpers(pyautogui)
    fast_lane = start_fast_lane(helpers, os.environ)
    jev_asker = start_jev_asker(helpers, os.environ)
    helpers.install(namespace)
    emit({"ready": True, "platform": sys.platform, "width": width, "height": height, "fast_lane": fast_lane,
          "jev_asker": jev_asker})
    if "--check" in sys.argv:
        return
    parent = os.getppid()

    def watch_parent() -> None:
        while True:
            time.sleep(1)
            if os.getppid() != parent:
                os.kill(os.getpid(), signal.SIGKILL)

    threading.Thread(target=watch_parent, daemon=True).start()
    serve(namespace, sys.stdin, helpers)


if __name__ == "__main__":
    if "--release" in sys.argv:
        import Quartz

        release_inputs(Quartz)
        sys.exit(0)
    try:
        main()
    except BaseException as error:  # noqa: BLE001 — the parent needs a protocol error for any startup failure
        emit({"error": {"code": "startup", "message": f"{type(error).__name__}: {error}"[:4000]}})
        sys.exit(1)
