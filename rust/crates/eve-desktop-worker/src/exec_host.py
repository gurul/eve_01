"""The exec host of eve-desktop-worker: the one part of the desktop worker that must be Python.

The planner model writes Python (`exec_py`), so its code runs in a Python interpreter. This child of the
Rust worker keeps the namespace (globals persist between calls, as in desktop_worker.py), runs each call
under the same rules (`<exec_py>` filename, stdout and stderr captured as text, every exception back as a
traceback, the PyAutoGUI fail-safe reported as terminal), and owns nothing else: the output list, its
size limits, the helpers, the auto-screenshot rule and the line protocol to the daemon are Rust.

Protocol: one JSON object per line on a private copy of stdin / stdout (fd 0 and 1 are pointed at
/dev/null and stderr, so a stray print or native write can never corrupt the pipe).

  Rust -> host, a request:   {"op": <name>, ...}            answered by {"t": "reply", "ok": v} | {"t": "reply", "err": [kind, message]}
  host -> Rust, during exec: {"t": "log" | "display" | "call" | "act" | "act_end", ...}
                             answered by {"ok": v} | {"err": [kind, message]}
                             and {"t": "done", ...} when the call ends.

While the host waits for an answer it serves nested requests (Rust's helpers drive PyAutoGUI through
{"op": "gui"} while a helper call from the model's code is in flight).
"""

import base64
import builtins
import contextlib
import importlib
import json
import os
import sys
import threading
import time
import traceback

_IN = os.fdopen(os.dup(0), "r", encoding="utf-8", newline="\n")
_OUT = os.fdopen(os.dup(1), "w", encoding="utf-8")
_null = os.open(os.devnull, os.O_RDONLY)
os.dup2(_null, 0)
os.close(_null)
os.dup2(2, 1)

NS = {"__builtins__": builtins}
_ORIGINALS = {}          # PyAutoGUI input calls, unwrapped, for the Rust helpers' own input
_STATE = {"gui": 0, "gui_exc": None}
_NAMED = {}              # exception classes named by the Rust side that builtins does not have


def _send(obj):
    # allow_nan=False: a NaN the model passed to a helper is its ValueError, not a line Rust cannot read
    _OUT.write(json.dumps(obj, default=_jsonable, allow_nan=False) + "\n")
    _OUT.flush()


def _recv():
    line = _IN.readline()
    if not line:
        os._exit(0)      # the worker is gone
    return json.loads(line)


def _jsonable(value):
    if isinstance(value, (bytes, bytearray)):
        return base64.b64encode(bytes(value)).decode("ascii")
    if hasattr(value, "__index__"):
        return value.__index__()
    if hasattr(value, "__float__"):
        return float(value)
    if hasattr(value, "__iter__"):
        return list(value)
    return str(value)


def _error(kind, message):
    """The exception the Rust side reported. A PyAutoGUI exception raised under a helper comes back as itself."""
    pending = _STATE["gui_exc"]
    if pending is not None and type(pending).__name__ == kind:
        _STATE["gui_exc"] = None
        return pending
    cls = getattr(builtins, kind, None)
    if not (isinstance(cls, type) and issubclass(cls, BaseException)):
        cls = _NAMED.get(kind)
        if cls is None:
            cls = _NAMED[kind] = type(kind, (Exception,), {"__module__": "eve_bridge.desktop_helpers"})
    return cls(message)


def _request(msg):
    """Send one message to the Rust side and wait for its answer, serving nested requests meanwhile."""
    _send(msg)
    while True:
        reply = _recv()
        if "op" in reply:
            _serve(reply)
            continue
        if "err" in reply:
            raise _error(*reply["err"])
        return reply.get("ok")


# ---- what the model's code sees ---------------------------------------------------------------------

def log(*values):
    _request({"t": "log", "text": " ".join(str(v) for v in values)})


def display(value):
    encode = 0.0
    try:
        from PIL import Image
    except ImportError:
        Image = None
    if Image is not None and isinstance(value, Image.Image):
        import io

        t0 = time.perf_counter()
        buf = io.BytesIO()
        value.save(buf, format="PNG")
        value = buf.getvalue()
        encode = time.perf_counter() - t0
    png = base64.b64encode(value).decode("ascii") if isinstance(value, bytes) else None
    _request({"t": "display", "png": png, "encode": encode})


class _Writer:
    def write(self, text):
        if text.strip():
            log(text.rstrip("\n"))
        return len(text)

    def flush(self):
        pass


def _call(helper, /, **kwargs):
    return _request({"t": "call", "name": helper, "kwargs": kwargs})


def click_element(x, y, expect="", clicks=1):
    return _call("click_element", x=x, y=y, expect=expect, clicks=clicks)


def open_app(name, wait=8.0):
    return _call("open_app", name=name, wait=wait)


def open_url(url, app=None, wait=8.0):
    return _call("open_url", url=url, app=app, wait=wait)


def frontmost():
    return _call("frontmost")


def screen_text(region=None, level="accurate"):
    return _call("screen_text", region=region, level=level)


def find_text(text, region=None, level="accurate"):
    return _call("find_text", text=text, region=region, level=level)


def click_text(text, region=None, timeout=3.0, clicks=1):
    return _call("click_text", text=text, region=region, timeout=timeout, clicks=clicks)


def wait_for(text, *, gone=False, timeout=6.0, interval=0.3, region=None):
    return _call("wait_for", text=text, gone=gone, timeout=timeout, interval=interval, region=region)


def wait_settled(timeout=5.0, interval=0.25):
    return _call("wait_settled", timeout=timeout, interval=interval)


def type_text(text, submit=False):
    return _call("type_text", text=text, submit=submit)


def zoom(x, y, w, h):
    return _call("zoom", x=x, y=y, w=w, h=h)


def observe(note=""):
    return _call("observe", note=note)


def delegate(objective="", text=None, key=None, done_when=None, approve=None, max_steps=4, dry_run=False,
             steps=None):
    # Python's own conversions, as desktop_helpers.Helpers.delegate makes them
    if steps is not None:
        if isinstance(steps, str):
            steps = [steps]
        steps = [str(s)[:200] for s in steps]
    else:
        objective = str(objective or "")[:200]
        text = str(text)[:200] if text is not None else None
        max_steps = int(max_steps)
    return _call("delegate", objective=objective, text=text, key=key, done_when=done_when, approve=approve,
                 max_steps=max_steps, dry_run=bool(dry_run), steps=steps)


HELPERS = {f.__name__: f for f in (click_element, open_app, open_url, frontmost, screen_text, find_text,
                                    click_text, wait_for, wait_settled, type_text, zoom, observe, delegate)}


def _acting(original):
    """desktop_helpers.Helpers._acting: the input call marks `acted` on the Rust side and is timed there."""
    name = getattr(original, "__name__", "action")

    def wrapped(*args, **kwargs):
        if _STATE["gui"]:
            return original(*args, **kwargs)     # the Rust helpers' own input: already marked there
        point = None
        if name in ("click", "doubleClick", "rightClick") and len(args) >= 2:
            try:
                point = [int(args[0]), int(args[1])]
            except BaseException:
                _request({"t": "act", "name": name, "point": None, "timed": False})
                raise
        _request({"t": "act", "name": name, "point": point, "timed": True})
        try:
            return original(*args, **kwargs)
        finally:
            _request({"t": "act_end"})

    wrapped._eve_wrapped = True
    wrapped.__name__ = name
    return wrapped


# ---- the requests ----------------------------------------------------------------------------------

def _op_init(req):
    for name in req.get("modules") or []:
        NS[name] = importlib.import_module(name)
    return {"platform": sys.platform, "executable": os.path.realpath(sys.executable)}


def _op_setup(req):
    exec(compile(req["code"], "<setup>", "exec"), NS)  # noqa: S102 - the worker's own setup lines


def _op_check_desktop(req):
    """desktop_worker.check_desktop's permission half, for this interpreter (it posts the input)."""
    if sys.platform != "darwin":
        return None
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
    return None


def _op_normalize_screenshots(req):
    """desktop_worker.normalize_screenshots: pyautogui.screenshot() in point coordinates (Retina 2x -> 1x)."""
    from PIL import Image

    pyautogui = NS.get("pyautogui")
    capture = pyautogui.screenshot

    def screenshot(imageFilename=None, region=None, **kwargs):
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


def _op_install(req):
    """desktop_helpers.Helpers.install: bind the helpers Rust names, drop the lane's when it is off, wrap input."""
    for name in req.get("names") or []:
        NS[name] = HELPERS[name]
    for name in req.get("optional") or []:
        if name not in (req.get("names") or []):
            NS.pop(name, None)
    gui = NS.get("pyautogui")
    for name in req.get("actions") or []:
        original = getattr(gui, name, None)
        if original is None or getattr(original, "_eve_wrapped", False):
            continue
        _ORIGINALS[name] = original
        setattr(gui, name, _acting(original))


def _op_gui(req):
    """One PyAutoGUI call on behalf of a Rust helper (it marked and timed the input itself)."""
    gui = NS.get("pyautogui")
    fn = _ORIGINALS.get(req["fn"]) or getattr(gui, req["fn"])
    _STATE["gui"] += 1
    try:
        result = fn(*(req.get("args") or []), **(req.get("kwargs") or {}))
    except BaseException as e:
        _STATE["gui_exc"] = e
        raise
    finally:
        _STATE["gui"] -= 1
    if req["fn"] == "size":
        return [int(result[0]), int(result[1])]
    return None


def _op_eval(req):
    return eval(req["expr"], NS)  # noqa: S307 - the worker's own probe (tests read the fake PyAutoGUI)


def _op_exec(req):
    """desktop_worker.execute's exec half: run one call; the Rust side has the output."""
    _STATE["gui_exc"] = None
    NS.update(log=log, display=display)
    try:
        with contextlib.redirect_stdout(_Writer()), contextlib.redirect_stderr(_Writer()):
            exec(compile(req["code"], "<exec_py>", "exec"), NS)  # noqa: S102 - the model's code, on purpose
    except BaseException as error:  # noqa: BLE001 - every error goes back to the model
        failsafe = getattr(NS.get("pyautogui"), "FailSafeException", ())
        if failsafe and isinstance(error, failsafe):
            return {"failsafe": True, "errored": False, "traceback": ""}
        return {"failsafe": False, "errored": True, "traceback": traceback.format_exc()[-4000:]}
    return {"failsafe": False, "errored": False, "traceback": ""}


_OPS = {"init": _op_init, "setup": _op_setup, "check_desktop": _op_check_desktop,
        "normalize_screenshots": _op_normalize_screenshots, "install": _op_install, "gui": _op_gui,
        "eval": _op_eval}


def _serve(req):
    op = req.get("op")
    if op == "exec":
        _send({"t": "done", **_op_exec(req)})
        return
    try:
        result = _OPS[op](req)
    except BaseException as e:  # noqa: BLE001 - the Rust side decides what an error means
        _send({"t": "reply", "err": [type(e).__name__, str(e)]})
        return
    try:
        line = json.dumps({"t": "reply", "ok": result}, default=_jsonable, allow_nan=False)
    except ValueError as e:
        _send({"t": "reply", "err": [type(e).__name__, str(e)]})
        return
    _OUT.write(line + "\n")
    _OUT.flush()


def _watch_parent(parent):
    while True:
        time.sleep(1)
        if os.getppid() != parent:
            os._exit(1)


threading.Thread(target=_watch_parent, args=(os.getppid(),), daemon=True).start()
_send({"t": "hello"})
while True:
    _serve(_recv())
