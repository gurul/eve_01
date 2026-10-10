"""On-device senses and effectors for the desktop worker: what the model's
exec_py code gets besides raw PyAutoGUI.

Every helper is a plain method on `Helpers`; `install()` binds them into the
worker's exec namespace and wraps the PyAutoGUI input functions so the worker
knows when a call acted (it then appends a settled screenshot, see
desktop_worker.execute). Each helper returns a value AND logs one short
sentence (≤ 80 chars, never the typed text) that the human reads on the robot.

Perception is local, so it costs 0.04–0.31 s instead of a model turn
(measured 2026-09-06 on a 3024x1964 display):

- `cg_capture`: CGDisplayCreateImage of the main display, 0.037 s, BGRA →
  a Pillow RGB image pixel-identical to pyautogui.screenshot() (0.114 s).
- `vision_ocr`: VNRecognizeTextRequest on that CGImage, accurate 0.31 s /
  fast 0.042 s (fast merges neighbouring words). Boxes are normalized with a
  bottom-left origin; `screen_text` maps them to click points.
- `ax_frontmost`: NSWorkspace frontmost app + the AX focused-window title.

Settle de-duplication: open_app / open_url settle inside the helper, and the
worker's auto-screenshot used to settle again (0.3 s typical, 1.5 s worst,
measured over the 2026-09 run logs). Every acted site bumps `_action_seq`; a
settle attempt that finished records (clock, seq), including a timeout on an
animated screen; `settled_screenshot` reuses that frame when no input happened
since and it is under SETTLE_REUSE_SECS old. Reuse does not claim it settled.

Timing: every sense and effector adds its wall time to `timing` (ms, reset by
`begin()`), so the worker can attach {"capture", "resize", "ocr", "ax",
"settle", "act", "decide", ...} to each reply and the agent can log it — before
this nothing local was timed in production. The keys are not disjoint: "settle"
is a wall time that contains its own captures, which also count under "capture";
read "exec" for the total and the others for where it went.

Only desktop_worker.main() and the tests import this module; pyobjc is
imported lazily inside the three backend functions so the tests run anywhere.
"""

from __future__ import annotations

import logging
import os
import re
import subprocess
import time
from dataclasses import dataclass
from datetime import datetime
from typing import Any, Callable, Optional, Sequence
from urllib.parse import urlsplit

log = logging.getLogger(__name__)

HELPER_NAMES = ("open_app", "open_url", "frontmost", "screen_text", "find_text", "click_text",
                "click_element", "wait_for", "wait_settled", "type_text", "zoom", "observe")
FAST_LANE_HELPERS = ("delegate",)   # bound by install() only when the fast lane is on
SETTLE_REUSE_SECS = 0.3             # a settled frame younger than this, with no input since, is the reply's picture
# Roles an AX hit-test may climb to before clicking (handyman's snap: coordinates
# only need to land INSIDE the control; the control absorbs the model's error).
AX_PRESSABLE_ROLES = frozenset({"AXButton", "AXLink", "AXCheckBox", "AXRadioButton", "AXMenuItem",
                                "AXMenuBarItem", "AXPopUpButton", "AXTab", "AXTextField", "AXTextArea",
                                "AXComboBox", "AXCell", "AXRow", "AXImage", "AXStaticText", "AXDisclosureTriangle",
                                "AXIncrementor", "AXSlider", "AXSearchField"})
AX_MAX_HOPS = 6
ACTION_NAMES = ("click", "doubleClick", "rightClick", "middleClick", "mouseDown", "mouseUp", "dragTo",
                "dragRel", "moveTo", "scroll", "hscroll", "press", "hotkey", "keyDown", "keyUp",
                "write", "typewrite")
BROWSER_BUNDLES = frozenset({"com.apple.Safari", "com.google.Chrome", "org.mozilla.firefox",
                             "company.thebrowser.Browser", "com.brave.Browser", "com.microsoft.edgemac"})
MAX_WAIT_SECS = 30.0          # every wait is clamped here: 30 s + 0.31 s OCR stays under the 60 s exec deadline
# Change detection on a 1/4-scale grey thumbnail (one thumb pixel = 2x2 points on Retina).
# Measured 2026-09-10 on a 3024x1964 frame: a 14x14-point checkbox toggle is ~63 changed
# thumb pixels here, and was 0.00022 of the old 1/8-scale image — under the 0.003 fraction
# the detector used to need, so it reported "unchanged". A pixel-count floor replaces the
# fraction: a text caret stays under it, a checkbox clears it.
THUMB_DIVISOR = 4
CHANGE_PIXEL_DELTA = 24       # a thumb pixel counts as changed above this grey-level difference
CHANGE_MIN_PIXELS = 24        # noise floor in thumb pixels: a caret is ~8-16, a checkbox ~50-60
MENU_BAR_POINTS = 40          # the menu-bar clock ticks every minute; that strip never counts
SCREEN_TEXT_MAX_LINES = 120
SCREEN_TEXT_MAX_BYTES = 6 * 1024
ZOOM_MAX_POINTS = (800, 600)
EMPTY_FRONT = {"app": "", "bundle": "", "title": "", "pid": 0}


@dataclass
class Frame:
    """One physical-pixel capture of the screen (2x on Retina)."""

    cg: Any                    # CGImageRef, or None in tests
    width: int
    height: int
    _pil: Any = None
    _thumb: Any = None

    @classmethod
    def from_pil(cls, img: Any) -> "Frame":
        return cls(None, int(img.width), int(img.height), img)

    def pil(self) -> Any:
        """The frame as a Pillow RGB image (cached)."""
        if self._pil is None:
            import Quartz
            from PIL import Image

            data = Quartz.CGDataProviderCopyData(Quartz.CGImageGetDataProvider(self.cg))
            bpr = Quartz.CGImageGetBytesPerRow(self.cg)
            self._pil = Image.frombuffer("RGBA", (self.width, self.height), bytes(data), "raw", "BGRA", bpr, 1)
            self._pil = self._pil.convert("RGB")
        return self._pil

    def thumb(self) -> Any:
        """1/4-scale grayscale, for change detection (~5 ms on a 3024x1964 frame)."""
        if self._thumb is None:
            w, h = max(1, self.width // THUMB_DIVISOR), max(1, self.height // THUMB_DIVISOR)
            self._thumb = self.pil().convert("L").resize((w, h))
        return self._thumb


def change_box(a_thumb: Any, b_thumb: Any, skip_top: int = 0,
               min_pixels: int = CHANGE_MIN_PIXELS) -> Optional[tuple[int, int, int, int]]:
    """Where two thumbnails differ: the bounding box (x, y, w, h) in thumb pixels, or None
    when fewer than `min_pixels` changed. Rows above `skip_top` never count. Thumbnails of
    different sizes differ everywhere."""
    from PIL import ImageChops

    if a_thumb.size != b_thumb.size:
        return (0, 0, int(b_thumb.width), int(b_thumb.height))
    diff = ImageChops.difference(a_thumb, b_thumb).point(lambda v: 255 if v > CHANGE_PIXEL_DELTA else 0)
    if skip_top > 0:
        diff.paste(0, (0, 0, diff.width, min(int(skip_top), diff.height)))
    if diff.histogram()[255] < min_pixels:
        return None
    x0, y0, x1, y1 = diff.getbbox()
    return (x0, y0, x1 - x0, y1 - y0)


def app_name_for_pid(pid: int) -> str:
    """The app that owns a process, "" when unknown. AppKit first (0.3 ms), then libproc (0.02 ms)."""
    try:
        from AppKit import NSRunningApplication

        app = NSRunningApplication.runningApplicationWithProcessIdentifier_(int(pid))
        if app is not None and app.localizedName():
            return str(app.localizedName())
    except Exception:  # noqa: BLE001
        pass
    try:
        import ctypes
        import ctypes.util

        lib = ctypes.CDLL(ctypes.util.find_library("proc"))
        buf = ctypes.create_string_buffer(256)
        if lib.proc_name(int(pid), buf, 256) > 0:
            return buf.value.decode("utf-8", errors="replace")
    except Exception:  # noqa: BLE001
        pass
    return ""


def _what(el: dict[str, Any]) -> str:
    """"AXButton 'Play' in Spotify" — the role, the title when there is one, and the owning app."""
    role, title, app = el.get("role") or "element", el.get("title") or "", el.get("app") or ""
    text = f"{role} {title!r}" if title else role
    return f"{text} in {app}" if app else text


# ---- real backends (macOS only) ------------------------------------------------------

def cg_capture() -> Frame:
    """The main display as a CGImage (physical pixels)."""
    import Quartz

    cg = Quartz.CGDisplayCreateImage(Quartz.CGMainDisplayID())
    if cg is None:
        raise RuntimeError("screen capture failed — Screen Recording is not granted to this python")
    return Frame(cg, int(Quartz.CGImageGetWidth(cg)), int(Quartz.CGImageGetHeight(cg)))


def vision_ocr(frame: Frame, level: str = "accurate") -> list[dict[str, Any]]:
    """Vision text recognition. Boxes are normalized, bottom-left origin: bx, by, bw, bh."""
    import Vision

    if frame.cg is None:
        raise RuntimeError("vision_ocr needs a CGImage frame")
    req = Vision.VNRecognizeTextRequest.alloc().init()
    fast = level == "fast"
    req.setRecognitionLevel_(Vision.VNRequestTextRecognitionLevelFast if fast
                             else Vision.VNRequestTextRecognitionLevelAccurate)
    req.setUsesLanguageCorrection_(False)
    handler = Vision.VNImageRequestHandler.alloc().initWithCGImage_options_(frame.cg, None)
    ok, err = handler.performRequests_error_([req], None)
    if not ok:
        raise RuntimeError(f"Vision OCR failed: {err}")
    out: list[dict[str, Any]] = []
    for obs in req.results() or []:
        cands = obs.topCandidates_(1)
        if not cands:
            continue
        box = obs.boundingBox()
        out.append({"text": str(cands[0].string()), "bx": float(box.origin.x), "by": float(box.origin.y),
                    "bw": float(box.size.width), "bh": float(box.size.height),
                    "conf": float(cands[0].confidence())})
    return out


def _ax_focused_pid() -> int:
    """The app that has keyboard focus, from AX — where a key press actually goes. 0 on error
    (kAXErrorCannotComplete, -25204, was seen once while a terminal held focus)."""
    try:
        from ApplicationServices import (
            AXUIElementCopyAttributeValue,
            AXUIElementCreateSystemWide,
            AXUIElementGetPid,
        )

        err, app = AXUIElementCopyAttributeValue(AXUIElementCreateSystemWide(), "AXFocusedApplication", None)
        if err != 0 or app is None:
            return 0
        err, pid = AXUIElementGetPid(app, None)
        return int(pid) if err == 0 and pid else 0
    except Exception:  # noqa: BLE001
        return 0


def _window_list_pid() -> int:
    """The owner of the frontmost normal (layer 0) on-screen window. 0 when there is none."""
    try:
        import Quartz

        opts = Quartz.kCGWindowListOptionOnScreenOnly | Quartz.kCGWindowListExcludeDesktopElements
        for w in Quartz.CGWindowListCopyWindowInfo(opts, Quartz.kCGNullWindowID) or []:
            if w.get("kCGWindowLayer") == 0 and w.get("kCGWindowOwnerPID"):
                return int(w["kCGWindowOwnerPID"])
    except Exception:  # noqa: BLE001
        pass
    return 0


def _workspace_pid() -> int:
    """NSWorkspace's idea of the frontmost app — last resort only, see `_focused_pid`."""
    try:
        from AppKit import NSWorkspace

        app = NSWorkspace.sharedWorkspace().frontmostApplication()
        return int(app.processIdentifier()) if app is not None else 0
    except Exception:  # noqa: BLE001
        return 0


def _focused_pid() -> int:
    """The frontmost app's pid, from sources that stay current in a long-lived process.

    NSWorkspace.frontmostApplication() is updated by notifications that only arrive
    while a run loop spins, and the desktop worker has none. Measured 2026-09-10 in a
    plain python process: after `open -a Finder` it still said Warp, while AX and the
    window list both said Finder. In a real run it said "Finder" for every step after
    Spotify came up, so open_app waited out its full 8 s twice and the model went
    hunting in the Dock.
    """
    for probe in (_ax_focused_pid, _window_list_pid, _workspace_pid):
        pid = probe()
        if pid:
            return pid
    return 0


def ax_frontmost() -> dict[str, Any]:
    """{"app","bundle","title","pid"} of the frontmost app; title "" on any AX error."""
    pid = _focused_pid()
    if not pid:
        return dict(EMPTY_FRONT)
    name, bundle = "", ""
    try:
        from AppKit import NSRunningApplication

        app = NSRunningApplication.runningApplicationWithProcessIdentifier_(pid)
        if app is not None:
            name, bundle = str(app.localizedName() or ""), str(app.bundleIdentifier() or "")
    except Exception:  # noqa: BLE001
        pass
    info: dict[str, Any] = {"app": name or app_name_for_pid(pid), "bundle": bundle, "title": "", "pid": pid}
    try:
        from ApplicationServices import (
            AXUIElementCopyAttributeValue,
            AXUIElementCreateApplication,
            kAXFocusedWindowAttribute,
            kAXTitleAttribute,
        )

        element = AXUIElementCreateApplication(info["pid"])
        err, window = AXUIElementCopyAttributeValue(element, kAXFocusedWindowAttribute, None)
        if err == 0 and window is not None:
            err, title = AXUIElementCopyAttributeValue(window, kAXTitleAttribute, None)
            if err == 0 and title:
                info["title"] = str(title)
    except Exception:  # noqa: BLE001 — a missing title is not an error for the model
        pass
    return info


def ax_element_at(x: int, y: int) -> Optional[dict[str, Any]]:
    """The accessibility element under a point (points, top-left origin), climbed to the
    nearest pressable ancestor: {"role","title","frame":(x,y,w,h),"pressable"}. None when
    nothing is there or Accessibility is not granted. Bench 2026-09-06: 1-24 ms."""
    try:
        from ApplicationServices import (
            AXUIElementCopyActionNames,
            AXUIElementCopyAttributeValue,
            AXUIElementCopyElementAtPosition,
            AXUIElementCreateSystemWide,
            AXUIElementGetPid,
            AXValueGetValue,
            kAXValueCGPointType,
            kAXValueCGSizeType,
        )
    except Exception:  # noqa: BLE001
        return None

    def attr(el: Any, name: str) -> Any:
        err, val = AXUIElementCopyAttributeValue(el, name, None)
        return val if err == 0 else None

    def describe(el: Any) -> dict[str, Any]:
        role = str(attr(el, "AXRole") or "")
        title = attr(el, "AXTitle") or attr(el, "AXDescription") or ""
        if not title:
            val = attr(el, "AXValue")
            if isinstance(val, str):
                title = val[:60]
        err, names = AXUIElementCopyActionNames(el, None)
        actions = list(names) if err == 0 and names else []
        frame = None
        pos, size = attr(el, "AXPosition"), attr(el, "AXSize")
        if pos is not None and size is not None:
            ok1, pt = AXValueGetValue(pos, kAXValueCGPointType, None)
            ok2, sz = AXValueGetValue(size, kAXValueCGSizeType, None)
            if ok1 and ok2:
                frame = (int(pt.x), int(pt.y), int(sz.width), int(sz.height))
        return {"role": role, "title": str(title), "frame": frame,
                "pressable": "AXPress" in actions or role in AX_PRESSABLE_ROLES}

    err, el = AXUIElementCopyElementAtPosition(AXUIElementCreateSystemWide(), float(x), float(y), None)
    if err != 0 or el is None:
        return None
    # Which app the point belongs to. A click aimed at Spotify that lands on Warp's
    # scroll area reads "AXScrollArea in Warp" — the wrong-app input the logs showed.
    app = ""
    try:
        perr, pid = AXUIElementGetPid(el, None)
        if perr == 0 and pid:
            app = app_name_for_pid(pid)
    except Exception:  # noqa: BLE001
        pass
    cur, first = el, None
    found = None
    for _ in range(AX_MAX_HOPS):
        d = describe(cur)
        first = first or d
        if d["pressable"] and (d["title"] or d["role"] != "AXGroup"):
            found = d
            break
        cur = attr(cur, "AXParent")
        if cur is None:
            break
    result = found or first
    if result is not None:
        result["app"] = app
    return result


# ---- AXPress on the element a snapshot saw (computah's fallback, 2026-10-06) ----------------------------
#
# The lane clicks with the mouse after a hit-test says the candidate is under its centre. When the hit-test
# misses (an overlay, a tooltip, a label drawn by a child the hit-test climbs past) it refused, and the step
# escalated to the planner. computah (github.com/musubipapi/computah, MIT, AXActor.swift) presses the SAME
# native element its observation read, through its AXPress action, under the same target checks; a run never
# sends both for one action and never repeats an input whose effect is unknown. Here: the element comes from
# the snapshot's walk (ax_candidates.Snapshot.ref), and is re-read now: same pid, enabled, AXPress offered,
# frame within 2 pt of the candidate's, and its own title, when it has one, the candidate's label.

AX_PRESS_ENV = "EVE_AX_PRESS"
AX_PRESS_DEFAULT = True              # it acts only where the lane would have refused; the lane itself ships off
AX_PRESS_SLACK = 2                   # points the element may have moved and still be the same control


def ax_press_on(environ: Any = None) -> bool:
    raw = str((os.environ if environ is None else environ).get(AX_PRESS_ENV, "") or "").strip().lower()
    if raw in ("0", "false", "no", "off"):
        return False
    if raw in ("1", "true", "yes", "on"):
        return True
    return AX_PRESS_DEFAULT


def ax_read(el: Any) -> Optional[dict[str, Any]]:
    """{"role","title","frame","enabled","pid","actions"} of a live element now, or None when it is gone."""
    try:
        from ApplicationServices import (
            AXUIElementCopyActionNames,
            AXUIElementCopyAttributeValue,
            AXUIElementGetPid,
            AXValueGetValue,
            kAXValueCGPointType,
            kAXValueCGSizeType,
        )
    except Exception:  # noqa: BLE001
        return None

    def attr(name: str) -> Any:
        err, val = AXUIElementCopyAttributeValue(el, name, None)
        return val if err == 0 else None

    role = attr("AXRole")
    if role is None:
        return None
    frame = None
    pos, size = attr("AXPosition"), attr("AXSize")
    if pos is not None and size is not None:
        ok1, pt = AXValueGetValue(pos, kAXValueCGPointType, None)
        ok2, sz = AXValueGetValue(size, kAXValueCGSizeType, None)
        if ok1 and ok2:
            frame = (int(pt.x), int(pt.y), int(sz.width), int(sz.height))
    err, names = AXUIElementCopyActionNames(el, None)
    perr, pid = AXUIElementGetPid(el, None)
    enabled = attr("AXEnabled")
    return {"role": str(role), "title": str(attr("AXTitle") or attr("AXDescription") or ""), "frame": frame,
            "enabled": True if enabled is None else bool(enabled), "pid": int(pid) if perr == 0 else 0,
            "actions": [str(n) for n in names] if err == 0 and names else []}


def ax_press(el: Any) -> int:
    """AXUIElementPerformAction(el, AXPress): the AXError code, 0 on success."""
    from ApplicationServices import AXUIElementPerformAction

    return int(AXUIElementPerformAction(el, "AXPress"))


def _matches(expect: str, role: str, title: str) -> bool:
    """Does the model's expectation ("the blue Sign in button") describe this element?"""
    e = expect.casefold()
    t = title.casefold()
    if t and (t in e or e in t):
        return True
    words = {w for w in re.findall(r"[a-z0-9]+", e) if len(w) > 2 and w not in ("the", "button", "link", "icon",
                                                                                   "field", "menu", "tab", "item")}
    hits = sum(1 for w in words if w in t)
    if words and hits / len(words) >= 0.5:
        return True
    # "the text field" against an untitled AXTextField: compare the role word without "AX"
    return not t and role[2:].casefold() in re.sub(r"\s+", "", e)


# ---- the helpers ----------------------------------------------------------------------

class Helpers:
    """The model-facing helpers. Every backend is injectable for the tests."""

    def __init__(
        self,
        pyautogui: Any,
        *,
        capture: Callable[[], Frame] = cg_capture,
        ocr: Callable[[Frame, str], list[dict[str, Any]]] = vision_ocr,
        frontmost_fn: Callable[[], dict[str, Any]] = ax_frontmost,
        element_at: Callable[[int, int], Optional[dict[str, Any]]] = ax_element_at,
        focused_element: Callable[[], Optional[dict[str, Any]]] = None,  # type: ignore[assignment]
        element_read: Callable[[Any], Optional[dict[str, Any]]] = ax_read,
        element_press: Callable[[Any], int] = ax_press,
        run: Callable[..., Any] = subprocess.run,
        clipboard: Any = None,
        clock: Callable[[], float] = time.monotonic,
        sleep: Callable[[float], None] = time.sleep,
        now: Callable[[], datetime] = datetime.now,
        menu_bar_points: int = MENU_BAR_POINTS,
        fast_lane: bool = False,
        decider: Any = None,
    ) -> None:
        self.gui = pyautogui
        self.fast_lane = fast_lane
        self.decider = decider                      # `model` mode's decider (decider.Decider over jev.py), or None
        self._menu_bar_points = menu_bar_points
        self._capture_fn = capture
        self._ocr = ocr
        self._frontmost_fn = frontmost_fn
        self._element_at = element_at
        self._focused_element = focused_element or ax_focused_element
        self._element_read = element_read
        self._element_press = element_press
        self.fast_lane_status = "off"           # what the worker reports: off | loading | ready (…) | failed: …
        self.lane_decide = "keyword"            # fast_lane.DECIDE_MODES: who picks a step (the worker sets it from env)
        self.step_asker: Any = None             # typed_ask.ask_jev_step bound to a predict (the worker sets it), or None
        self.lane_first_on = False              # the router (lane_router.py) may run before the planner
        self._run = run
        self._last_click: Optional[str] = None      # "AXButton 'Search'" under the last raw click
        self._clipboard = clipboard
        self._clock = clock
        self._sleep = sleep
        self._now = now
        self.acted = False
        self.log: Callable[..., None] = lambda *_a: None
        self.display: Callable[[Any], None] = lambda _i: None
        self._frame: Optional[Frame] = None
        self._last_thumb: Any = None
        self._elapsed = 0.0
        self._action_seq = 0                        # bumped by every acted site
        self._settled: Optional[tuple[float, int]] = None   # (clock, action_seq) when a settle last finished
        self.timing: dict[str, float] = {}          # ms per sense/effector for the current exec call

    # -- wiring --
    def install(self, namespace: dict[str, Any]) -> None:
        """Bind the helpers into the exec namespace and make every PyAutoGUI input call mark `acted`."""
        for name in HELPER_NAMES:
            namespace[name] = getattr(self, name)
        for name in FAST_LANE_HELPERS:
            if self.fast_lane:
                namespace[name] = getattr(self, name)
            else:
                namespace.pop(name, None)
        for name in ACTION_NAMES:
            original = getattr(self.gui, name, None)
            if original is None or getattr(original, "_eve_wrapped", False):
                continue
            setattr(self.gui, name, self._acting(original))

    def _acting(self, original: Callable[..., Any]) -> Callable[..., Any]:
        name = getattr(original, "__name__", "action")

        def wrapped(*args: Any, **kwargs: Any) -> Any:
            self._mark_acted()
            if name in ("click", "doubleClick", "rightClick") and len(args) >= 2:
                # What is under a raw click, for the [after] line (handyman-style verification
                # of the model's coordinates — it sees "clicked on AXLink 'Weather'").
                self._last_click = self._describe_point(int(args[0]), int(args[1]))
            t0 = self._clock()
            try:
                return original(*args, **kwargs)
            finally:
                self._add_timing("act", t0)

        wrapped._eve_wrapped = True  # type: ignore[attr-defined]
        wrapped.__name__ = getattr(original, "__name__", "action")
        return wrapped

    def bind(self, log: Callable[..., None], display: Callable[[Any], None]) -> None:
        self.log, self.display = log, display

    def begin(self) -> None:
        self.acted = False
        self._last_click = None
        self.timing = {}

    def _mark_acted(self) -> None:
        """Every site that clicks, types or presses keys: `acted` for the auto-screenshot, the
        sequence number so a settle from before this input is never reused after it."""
        self.acted = True
        self._action_seq += 1

    def _add_timing(self, key: str, t0: float) -> None:
        self.timing[key] = self.timing.get(key, 0.0) + (self._clock() - t0) * 1000.0

    def timing_ms(self) -> dict[str, float]:
        """The current call's timing, rounded, for the worker reply."""
        return {k: round(v, 2) for k, v in self.timing.items()}

    def _front(self) -> dict[str, Any]:
        t0 = self._clock()
        try:
            return self._frontmost_fn()
        finally:
            self._add_timing("ax", t0)

    def _describe_point(self, x: int, y: int) -> Optional[str]:
        t0 = self._clock()
        try:
            el = self._element_at(x, y)
        except Exception:  # noqa: BLE001 — a hit-test failure only costs the [after] detail
            return None
        finally:
            self._add_timing("ax", t0)
        if not el:
            return None
        return _what(el)

    def click_element(self, x: int, y: int, expect: str = "", clicks: int = 1) -> str:
        """Click the control under (x, y) — snapped to its centre — after checking it is the
        one you meant. `expect` is a few words from the screenshot ("Sign in button").
        A mismatch does NOT click; it tells you what is there instead."""
        el = None
        t0 = self._clock()
        try:
            el = self._element_at(int(x), int(y))
        except Exception:  # noqa: BLE001 — no element means a raw click, reported as such
            el = None
        finally:
            self._add_timing("ax", t0)
        if el is None:
            self._mark_acted()
            self.gui.click(int(x), int(y), clicks=clicks)
            line = f"clicked at ({x},{y}) (no accessibility element there to check)"
            self.log(line)
            return line
        role, title = el.get("role") or "element", el.get("title") or ""
        what = _what(el)
        if expect and not _matches(expect, role, title):
            line = f"did not click: under ({x},{y}) is {what}, not {expect!r}. Look again (screen_text / zoom)."
            self.log(line)
            return line
        cx, cy = int(x), int(y)
        frame = el.get("frame")
        if frame and frame[2] > 0 and frame[3] > 0:
            cx, cy = int(frame[0] + frame[2] / 2), int(frame[1] + frame[3] / 2)
        self._mark_acted()
        self._last_click = what
        self.gui.click(cx, cy, clicks=clicks)
        line = f"clicked {what} at ({cx},{cy})"
        self.log(line)
        return line

    # -- apps and pages --
    def open_app(self, name: str, wait: float = 8.0) -> str:
        """Launch or focus an app with `open -a`, then wait until it is frontmost."""
        self._mark_acted()
        if "/" in name or name.startswith("-"):
            raise ValueError(f"open_app: {name!r} must be an app name, not a path or an option")
        result = self._run(["open", "-a", name], capture_output=True, timeout=10)
        if result.returncode != 0:
            raise RuntimeError(f"no app called {name!r}")
        wait = min(float(wait), MAX_WAIT_SECS)
        t0 = self._clock()
        front = self._front()
        while front.get("app", "").casefold() != name.casefold() and self._clock() - t0 < wait:
            self._sleep(0.1)
            front = self._front()
        elapsed = self._clock() - t0
        if front.get("app", "").casefold() == name.casefold():
            text = f"opened {name} (frontmost after {elapsed:.1f} s)"
        else:
            text = f"opened {name} but {front.get('app') or 'something else'} is still frontmost after {wait:.1f} s"
        self._settle(1.5)
        self.log(text)
        return text

    def open_url(self, url: str, app: Optional[str] = None, wait: float = 8.0) -> str:
        """Open a URL in the default browser (or `app`) and wait for it to come up."""
        self._mark_acted()
        if urlsplit(url).scheme.lower() not in ("http", "https", "mailto"):
            raise ValueError("open_url: only http, https and mailto URLs")
        if app is not None and ("/" in app or app.startswith("-")):
            raise ValueError(f"open_url: {app!r} must be an app name")
        before = self._front()
        result = self._run(["open", *(["-a", app] if app else []), url], capture_output=True, timeout=10)
        if result.returncode != 0:
            raise RuntimeError(f"could not open {url!r}" + (f" in {app!r}" if app else ""))
        wait = min(float(wait), MAX_WAIT_SECS)
        t0 = self._clock()

        def arrived(front: dict[str, Any]) -> bool:
            # A named app must itself come to the front: another browser being up
            # is not arrival (bench 2026-09-06: Chrome in front made Safari "done").
            if app:
                return front.get("app", "").casefold() == app.casefold()
            if front.get("bundle") in BROWSER_BUNDLES:
                return True
            return front.get("title", "") != before.get("title", "")

        front = self._front()
        while not arrived(front) and self._clock() - t0 < wait:
            self._sleep(0.2)
            front = self._front()
        if arrived(front):
            text = f"opened {url} in {front.get('app') or 'the browser'}"
        else:
            text = f"opened {url} but {front.get('app') or 'something else'} is still frontmost after {wait:.1f} s"
        self._settle(2.0)
        self.log(text)
        return text

    def frontmost(self) -> dict[str, Any]:
        front = self._front()
        self.log(f"frontmost: {front.get('app', '')} — {front.get('title', '')!r}")
        return front

    # -- reading the screen --
    def screen_text(self, region: Optional[tuple[int, int, int, int]] = None,
                    level: str = "accurate") -> list[dict[str, Any]]:
        """OCR the screen: [{"text","x","y","w","h","conf"}] in click coordinates, top to bottom."""
        lines = self._lines(region, level)
        total = len(lines)
        kept: list[dict[str, Any]] = []
        size = 0
        for item in lines:
            size += len(item["text"].encode("utf-8")) + 48
            if len(kept) >= SCREEN_TEXT_MAX_LINES or size > SCREEN_TEXT_MAX_BYTES:
                break
            kept.append(item)
        if len(kept) < total:
            kept.append({"text": f"({total - len(kept)} more lines; use region=)", "x": 0, "y": 0, "w": 0, "h": 0,
                         "conf": 0.0})
        self.log(f"screen_text: {total} lines")
        return kept

    def _lines(self, region: Optional[tuple[int, int, int, int]], level: str) -> list[dict[str, Any]]:
        frame = self._capture()
        w_pts, h_pts = self._size()
        items: list[dict[str, Any]] = []
        t0 = self._clock()
        try:
            boxes = self._ocr(frame, level)
        finally:
            self._add_timing("ocr", t0)
        for r in boxes:
            x = round(r["bx"] * w_pts)
            y = round((1.0 - r["by"] - r["bh"]) * h_pts)
            w = round(r["bw"] * w_pts)
            h = round(r["bh"] * h_pts)
            if region is not None:
                rx, ry, rw, rh = region
                cx, cy = x + w / 2, y + h / 2
                if not (rx <= cx <= rx + rw and ry <= cy <= ry + rh):
                    continue
            items.append({"text": str(r["text"]), "x": x, "y": y, "w": w, "h": h,
                          "conf": round(float(r.get("conf", 1.0)), 3)})
        items.sort(key=lambda i: (i["y"] // 8, i["x"]))
        return items

    def find_text(self, text: str, region: Optional[tuple[int, int, int, int]] = None,
                  level: str = "accurate") -> Optional[dict[str, Any]]:
        """The first on-screen line equal to `text` (else containing it), with its centre cx, cy."""
        item = self._find(text, region, level)
        if item is None:
            self.log(f"{text!r} is not on screen")
        else:
            self.log(f"found {text!r} at ({item['cx']},{item['cy']})")
        return item

    def _find(self, text: str, region: Optional[tuple[int, int, int, int]], level: str) -> Optional[dict[str, Any]]:
        needle = text.casefold().strip()
        if not needle:
            return None
        lines = self._lines(region, level)
        hit = next((i for i in lines if i["text"].casefold().strip() == needle), None)
        if hit is None:
            hit = next((i for i in lines if needle in i["text"].casefold()), None)
        if hit is None:
            return None
        return {**hit, "cx": hit["x"] + hit["w"] // 2, "cy": hit["y"] + hit["h"] // 2}

    def click_text(self, text: str, region: Optional[tuple[int, int, int, int]] = None,
                   timeout: float = 3.0, clicks: int = 1) -> str:
        """Find `text` with accurate OCR (waiting up to `timeout`) and click its centre.

        Fast OCR can miss a label that screen_text() just read, so it must not
        gate a click behind repeated false negatives and another planner turn.
        """
        item = self._wait_for(text, gone=False, timeout=timeout, interval=0.3, region=region,
                              level="accurate")
        if not isinstance(item, dict):
            raise LookupError(f"{text!r} is not on screen")
        self._mark_acted()
        self.gui.click(item["cx"], item["cy"], clicks=clicks)
        line = f"clicked {text!r} at ({item['cx']},{item['cy']})"
        self.log(line)
        return line

    def wait_for(self, text: str, *, gone: bool = False, timeout: float = 6.0, interval: float = 0.3,
                 region: Optional[tuple[int, int, int, int]] = None) -> Any:
        """Poll (fast OCR) until `text` is on screen — or, with gone=True, until it has gone."""
        result = self._wait_for(text, gone=gone, timeout=timeout, interval=interval, region=region)
        elapsed = self._elapsed
        timeout = min(float(timeout), MAX_WAIT_SECS)
        if gone:
            self.log(f"{text!r} went away after {elapsed:.1f} s" if result else
                     f"{text!r} is still on screen after {timeout:.1f} s")
        else:
            self.log(f"{text!r} appeared after {elapsed:.1f} s" if result is not None else
                     f"{text!r} not seen in {timeout:.1f} s")
        return result

    def _wait_for(self, text: str, *, gone: bool, timeout: float, interval: float,
                  region: Optional[tuple[int, int, int, int]], level: str = "fast") -> Any:
        timeout = min(float(timeout), MAX_WAIT_SECS)
        t0 = self._clock()
        while True:
            item = self._find(text, region, level)
            self._elapsed = self._clock() - t0
            if gone and item is None:
                return True
            if not gone and item is not None:
                return (self._find(text, region, "accurate") or item) if level == "fast" else item
            if self._clock() - t0 >= timeout:
                return False if gone else None
            self._sleep(interval)

    def wait_settled(self, timeout: float = 5.0, interval: float = 0.25) -> bool:
        """Wait until two consecutive captures `interval` apart look the same."""
        settled = self._settle(timeout, interval)
        if settled:
            self.log(f"screen settled after {self._elapsed:.1f} s")
        else:
            self.log(f"screen still changing after {min(float(timeout), MAX_WAIT_SECS):.1f} s")
        return settled

    def _settle(self, timeout: float, interval: float = 0.25) -> bool:
        """Two captures `interval` apart that look the same; False past `timeout`.
        Remember a completed attempt, including timeout, for the auto-screenshot.
        """
        timeout = min(float(timeout), MAX_WAIT_SECS)
        t0 = self._clock()
        try:
            return self._settle_loop(timeout, interval, t0)
        finally:
            self._add_timing("settle", t0)

    def _settle_loop(self, timeout: float, interval: float, t0: float) -> bool:
        frame = self._capture()
        a = frame.thumb()
        skip = self._skip_rows(frame)
        if timeout <= 0:
            self._elapsed = 0.0
            return True
        while True:
            self._sleep(interval)
            b = self._capture().thumb()
            self._elapsed = self._clock() - t0
            if change_box(a, b, skip) is None:
                self._settled = (self._clock(), self._action_seq)
                return True
            a = b
            if self._clock() - t0 >= timeout:
                self._settled = (self._clock(), self._action_seq)
                return False

    # -- typing --
    def type_text(self, text: str, submit: bool = False) -> str:
        """Type any text (clipboard paste: emoji and accents survive), optionally pressing enter."""
        self._mark_acted()
        clipboard = self._clipboard
        if clipboard is None:
            import pyperclip

            clipboard = self._clipboard = pyperclip
        try:
            old = clipboard.paste()
        except Exception:  # noqa: BLE001 — a non-text clipboard is not worth failing over
            old = ""
        clipboard.copy(text)
        self.gui.hotkey("command", "v")
        self._sleep(0.15)
        clipboard.copy(old if isinstance(old, str) else "")
        line = f"typed {len(text)} characters"
        if submit:
            self.gui.press("enter")
            line += " and pressed enter"
        self.log(line)
        return line

    # -- looking --
    def zoom(self, x: int, y: int, w: int, h: int) -> str:
        """Display a close-up of that point region at the frame's physical scale."""
        frame = self._capture()
        w_pts, h_pts = self._size()
        scale = frame.width / float(w_pts) if w_pts else 1.0
        x, y = max(0, int(x)), max(0, int(y))
        w = max(1, min(int(w), ZOOM_MAX_POINTS[0], w_pts - x))
        h = max(1, min(int(h), ZOOM_MAX_POINTS[1], h_pts - y))
        box = (round(x * scale), round(y * scale), round((x + w) * scale), round((y + h) * scale))
        self.display(frame.pil().crop(box))
        line = (f"zoom of points ({x},{y},{w},{h}) at {scale:g}x: a pixel (px,py) in this image is point "
                f"(x+px/{scale:g}, y+py/{scale:g})")
        self.log(line)
        return line

    def observe(self, note: str = "") -> str:
        """A fresh screenshot plus the context line."""
        self.display(self.screenshot())
        line = self.context_line()
        if note:
            line += f" — {note}"
        self.log(line)
        return line

    # -- used by the worker --
    def screenshot(self) -> Any:
        """The screen in points, like pyautogui.screenshot() but from a CG capture."""
        return self._points(self._capture())

    def settled_screenshot(self, max_wait: float) -> Any:
        """Wait (up to max_wait) for the screen to stop changing, then the screenshot of that last frame.

        When a helper already waited after the last input (open_app, open_url,
        wait_settled) and that frame is under SETTLE_REUSE_SECS old, reuse it even
        if animation exhausted the wait. A timeout still returns False to its caller.
        """
        if max_wait > 0 and not self._settle_is_fresh():
            self._settle(max_wait)
        elif max_wait <= 0:
            self._capture()
        assert self._frame is not None
        return self._points(self._frame)

    def _settle_is_fresh(self) -> bool:
        if self._settled is None or self._frame is None:
            return False
        at, seq = self._settled
        return seq == self._action_seq and 0.0 <= self._clock() - at < SETTLE_REUSE_SECS

    # -- the fast lane --
    def delegate(self, objective: str = "", text: Optional[str] = None, key: Optional[str] = None,
                 done_when: Optional[str] = None, approve: Optional[str] = None, max_steps: int = 4,
                 dry_run: bool = False, steps: Optional[Sequence[str]] = None) -> str:
        """Hand a narrow run of clicks on labelled controls in the frontmost app to the lane.

        `steps=[…]` is an ordered list of exact labels, one click each (fast_lane.run_script);
        otherwise `objective` is one run (fast_lane.run_delegate). Who decides is
        `self.lane_decide` (fast_lane.DECIDE_MODES). Returns the one-line result the model reads;
        every step and the final line are logged for the human."""
        from .fast_lane import run_delegate, run_script

        adapter = _LaneAdapter(self, approve)
        if steps is not None:
            if isinstance(steps, str):
                steps = [steps]
            script = run_script([str(s)[:200] for s in steps], senses=adapter, effectors=adapter,
                                decider=self.decider, decide=self._script_decide(), approve=approve,
                                dry_run=bool(dry_run), clock=self._clock)
            for result in script.results:
                for step in result.steps:
                    self.timing["decide"] = self.timing.get("decide", 0.0) + float(step.decide_ms)
            for line in script.log_lines:
                self.log(line)
            return script.line
        objective = str(objective or "")[:200]
        text = str(text)[:200] if text is not None else None
        max_steps = max(1, min(6, int(max_steps)))
        result = run_delegate(objective, senses=adapter, effectors=adapter, decider=self.decider, text=text,
                              key=key, done_when=done_when, approve=approve, max_steps=max_steps,
                              dry_run=bool(dry_run), clock=self._clock, decide=self._lane_decide(),
                              asker=self.step_asker)
        for step in result.steps:
            self.timing["decide"] = self.timing.get("decide", 0.0) + float(step.decide_ms)
        for line in result.log_lines:
            self.log(line)
        return result.line

    def _lane_decide(self) -> str:
        """"jev" only while an asker exists: a hosted model that is not configured must not turn the
        lane off, so the keyword gate decides alone instead."""
        return "keyword" if self.lane_decide == "jev" and self.step_asker is None else self.lane_decide

    def _script_decide(self) -> str:
        """A script's steps are exact labels the planner read off the screen: the keyword gate's case."""
        return "keyword" if self.lane_decide == "jev" else self.lane_decide

    def outline(self, max_lines: int = 80) -> dict[str, Any]:
        """The front window as the planner is shown it: `label (role, value)` per pressable or editable
        control, in the lane's own ranking order — never the window title, never a field's contents."""
        from .ax_candidates import ax_snapshot, is_sensitive, rank_candidates
        from .decider import render_option

        snap = ax_snapshot(None, screen=self._size())
        pressable, _ = rank_candidates(snap, "", max_out=max_lines, kinds="pressable", allow_sensitive=True)
        fields, _ = rank_candidates(snap, "", max_out=10, kinds="editable")
        lines = [render_option(c) + (" [asks the human first]" if is_sensitive(c.label) else "") for c in pressable]
        lines += [f"{' '.join(c.label.split())[:40] or 'unlabelled'} ({c.role}, a text field)" for c in fields
                  if not c.secure]
        return {"app": snap.app, "lines": lines[:max_lines], "dialog": bool(snap.dialog_text),
                "truncated": bool(snap.truncated)}

    def run_plan(self, plan: dict[str, Any], request: str, start: int = 0,
                 approved: Optional[dict[str, str]] = None, dry_run: bool = False) -> dict[str, Any]:
        """Walk a plan_contract plan over this desktop (plan_executor.run_plan). Never raises."""
        from .plan_contract import PlanError, parse_plan
        from .plan_executor import PlanResult
        from .plan_executor import run_plan as walk

        try:
            parsed = parse_plan(plan, str(request or "")[:600], source=str(plan.get("source") or "astra"))
        except PlanError as e:
            return PlanResult("unavailable", reason=f"bad plan: {e}"[:200]).to_dict()
        adapter = _LaneAdapter(self, None)
        try:
            result = walk(parsed, senses=adapter, effectors=adapter, asker=self.step_asker,
                          open_app=self.open_app, open_url=self.open_url,
                          frontmost_app=lambda: str(self._front().get("app") or ""), start=max(0, int(start)),
                          approved={int(k): str(v) for k, v in (approved or {}).items()},
                          decide="jev" if self.step_asker is not None else "keyword",
                          dry_run=bool(dry_run), planned_for=str(plan.get("app") or "")[:80], clock=self._clock)
        except Exception as e:  # noqa: BLE001 — an executor that fails is "the planner does it", never a dead task
            result = PlanResult("partial", next_index=max(0, int(start)), reason=f"error:{type(e).__name__}: {e}"[:200])
        for line in result.log_lines:
            self.log(line)
        return result.to_dict()

    def lane_first(self, goal: str, dry_run: bool = False) -> dict[str, Any]:
        """The router (lane_router.route) over this desktop: try to finish `goal` with keyword-decided
        clicks before any planner call. Returns RouteResult.to_dict(); never raises."""
        from .lane_router import RouteResult, route

        t0 = self._clock()
        try:
            adapter = _LaneAdapter(self, None)
            front = str(self._front().get("app") or "")
            result = route(str(goal or "")[:400], senses=adapter, effectors=adapter, frontmost_app=front,
                           dry_run=bool(dry_run), clock=self._clock)
        except Exception as e:  # noqa: BLE001 — a router that fails is "the planner does it", never a dead task
            result = RouteResult("none", reason=f"error:{type(e).__name__}", ms=(self._clock() - t0) * 1000.0)
        for line in result.log_lines:
            self.log(line)
        return result.to_dict()

    def decisions_first(self, goal: str, client: Any, dry_run: bool = False) -> dict[str, Any]:
        """The opt-in visual loop, using the same hit-test and AXPress effectors as the keyword lane."""
        from .decision_lane import route

        adapter = _LaneAdapter(self, None)
        result = route(goal, senses=adapter, effectors=adapter, client=client, dry_run=dry_run)
        for line in result.log_lines:
            self.log(line)
        return result.to_dict()

    def context_line(self) -> str:
        front = self._front()
        w_pts, h_pts = self._size()
        return (f"frontmost: {front.get('app', '')} — {front.get('title', '')!r}; "
                f"screen {w_pts}x{h_pts}; {self._now():%H:%M}")

    def after_line(self) -> str:
        """What changed since the previous exec ended, and where, in click coordinates.

        "[after your input]" when this step clicked, typed or pressed keys, "[after]" when it
        only looked — so "[after your input] … screen: unchanged" means the input did nothing
        visible, which the agent uses to gate its repeat note and its final-answer check.
        """
        frame = self._capture()
        thumb = frame.thumb()
        box = None
        if self._last_thumb is not None:
            box = change_box(self._last_thumb, thumb, self._skip_rows(frame))
        self._last_thumb = thumb
        if box is None:
            screen = "unchanged"
        else:
            scale = self._thumb_scale(frame)
            x, y, w, h = (round(v * scale) for v in box)
            w_pts, h_pts = self._size()
            if w_pts and h_pts and w * h >= 0.6 * w_pts * h_pts:
                screen = "changed (most of the screen)"
            else:
                screen = f"changed around ({x},{y},{w},{h})"
        front = self._front()
        head = "[after your input]" if self.acted else "[after]"
        tail = f"; clicked on {self._last_click}" if self._last_click else ""
        return (f"{head} frontmost: {front.get('app', '')} — {front.get('title', '')!r}; "
                f"screen: {screen}{tail}")

    # -- internals --
    def _thumb_scale(self, frame: Frame) -> float:
        """Points per thumb pixel (2.0 on a 2x Retina frame)."""
        w_pts, _ = self._size()
        return w_pts * THUMB_DIVISOR / float(frame.width) if frame.width else 1.0

    def _skip_rows(self, frame: Frame) -> int:
        """Thumb rows covered by the menu bar, which never count as a change."""
        if self._menu_bar_points <= 0:
            return 0
        return int(self._menu_bar_points / self._thumb_scale(frame)) + 1

    def _capture(self) -> Frame:
        t0 = self._clock()
        try:
            self._frame = self._capture_fn()
        finally:
            self._add_timing("capture", t0)
        return self._frame

    def _size(self) -> tuple[int, int]:
        w, h = self.gui.size()
        return int(w), int(h)

    def _points(self, frame: Frame) -> Any:
        from PIL import Image

        t0 = self._clock()
        img = frame.pil()
        size = self._size()
        if tuple(img.size) != size:
            img = img.resize(size, Image.Resampling.LANCZOS)
        self._add_timing("resize", t0)
        return img


# ---- the fast lane's senses and effectors ---------------------------------------------------

def ax_focused_element() -> Optional[dict[str, Any]]:
    """The element that has keyboard focus right now: {"role","title","frame","app","editable"},
    or None. Read fresh every call — the lane checks it right after a click, before pasting."""
    try:
        from ApplicationServices import (
            AXUIElementCopyAttributeValue,
            AXUIElementCreateSystemWide,
            AXUIElementGetPid,
            AXValueGetValue,
            kAXValueCGPointType,
            kAXValueCGSizeType,
        )
    except Exception:  # noqa: BLE001 — no pyobjc, no focus check (the effector then refuses)
        return None

    def attr(el: Any, name: str) -> Any:
        err, val = AXUIElementCopyAttributeValue(el, name, None)
        return val if err == 0 else None

    err, el = AXUIElementCopyAttributeValue(AXUIElementCreateSystemWide(), "AXFocusedUIElement", None)
    if err != 0 or el is None:
        return None
    role = str(attr(el, "AXRole") or "")
    title = attr(el, "AXTitle") or attr(el, "AXDescription") or ""
    frame = None
    pos, size = attr(el, "AXPosition"), attr(el, "AXSize")
    if pos is not None and size is not None:
        ok1, pt = AXValueGetValue(pos, kAXValueCGPointType, None)
        ok2, sz = AXValueGetValue(size, kAXValueCGSizeType, None)
        if ok1 and ok2:
            frame = (int(pt.x), int(pt.y), int(sz.width), int(sz.height))
    app = ""
    try:
        perr, pid = AXUIElementGetPid(el, None)
        if perr == 0 and pid:
            app = app_name_for_pid(pid)
    except Exception:  # noqa: BLE001
        pass
    editable = role in ("AXTextField", "AXTextArea", "AXComboBox", "AXSearchField")
    return {"role": role, "title": str(title), "frame": frame, "app": app, "editable": editable}


def _norm(text: str) -> str:
    return " ".join(str(text or "").split()).casefold()


class _LaneAdapter:
    """fast_lane's senses and effectors over one Helpers (see fast_lane's module docstring for
    the protocol). Every input goes through the same wrapped PyAutoGUI calls as the model's own
    code, so `acted`, the timing dict and the auto-screenshot rule all apply."""

    def __init__(self, helpers: "Helpers", approve: Optional[str]) -> None:
        self.h = helpers
        self.approve = _norm(approve) if approve else ""
        self._thumb0: Any = None                    # the screen at the latest snapshot
        self._thumb_before: Any = None              # the screen at the snapshot before that: the step's start
        self._snapshot: Any = None

    # -- senses --
    def observe(self) -> tuple[Any, Any]:
        """Capture AX and screen together, returning a detached image in screen points.

        The loop reobserves both before acting: neither half is assumed atomic with the other.
        """
        snap = self.snapshot()
        frame = self.h._frame
        if frame is None:
            raise RuntimeError("no screen frame")
        return snap, self.h._points(frame).copy()

    @staticmethod
    def image_matches(before: Any, after: Any) -> bool:
        """Any pixel change below the macOS menu bar invalidates a pending visual decision."""
        from PIL import ImageChops

        if before.size != after.size or before.height <= 32:
            return False
        box = (0, 32, before.width, before.height)
        return ImageChops.difference(before.convert("RGB").crop(box), after.convert("RGB").crop(box)).getbbox() is None

    def snapshot(self) -> Any:
        from .ax_candidates import ax_snapshot

        t0 = self.h._clock()
        try:
            snap = ax_snapshot(None, screen=self.h._size())
        finally:
            self.h._add_timing("ax", t0)
        self._snapshot = snap
        self._thumb_before = self._thumb0
        try:
            self._thumb0 = self.h._capture().thumb()
        except Exception:  # noqa: BLE001 — no baseline means screen_changed() answers None
            self._thumb0 = None
            self.h._frame = None
        return snap

    def text_visible(self, s: str) -> bool:
        needle = _norm(s)
        if not needle:
            return False
        snap = self._snapshot
        if snap is not None:
            for c in snap.elements:
                if needle in _norm(c.label) or needle in _norm(c.value):
                    return True
            if needle in _norm(snap.title):
                return True
        try:
            lines = self.h._lines(None, "fast")
        except Exception:  # noqa: BLE001 — no OCR, no sighting
            return False
        return any(needle in _norm(ln["text"]) for ln in lines)

    def focused(self) -> Optional[Any]:
        el = self.h._focused_element()
        if not el:
            return None
        from .ax_candidates import Candidate, _role_name

        frame = el.get("frame") or (0, 0, 0, 0)
        role = _role_name(el.get("role") or "", "")
        return Candidate(id="focused", role=role, label=el.get("title") or "", value="", frame=tuple(frame),
                         actions=(), enabled=True, app=el.get("app") or "", in_content=False, in_dialog=False,
                         secure=False, editable=bool(el.get("editable")))

    def frontmost_pid(self) -> int:
        try:
            return int(self.h._front().get("pid") or 0)
        except Exception:  # noqa: BLE001
            return 0

    def screen_changed(self) -> Optional[bool]:
        """Did the screen change since the step began? The lane asks this AFTER its post-click
        snapshot, so the step's start is the snapshot before the latest one. Until 2026-09-21 this
        compared the latest snapshot's frame with a fresh capture — the settled screen with itself —
        and so could never see a change: only a control whose AX value flips (a radio button) was
        ever verified, and a plain button ("next year") always read as unknown."""
        if self._thumb0 is None:
            return None
        try:
            if self._thumb_before is not None:
                frame = self.h._frame or self.h._capture()
                return change_box(self._thumb_before, self._thumb0, self.h._skip_rows(frame)) is not None
            frame = self.h._capture()
            return change_box(self._thumb0, frame.thumb(), self.h._skip_rows(frame)) is not None
        except Exception:  # noqa: BLE001
            return None

    # -- effectors --
    def _centre(self, c: Any) -> tuple[int, int]:
        x, y, w, h = c.frame
        return int(x + w / 2), int(y + h / 2)

    @staticmethod
    def _same_control(el: dict[str, Any], c: Any) -> bool:
        """Is the hit-tested element the candidate? Equal titles, or — for a row / cell / link whose
        label came from a descendant static text (System Settings, Finder, Music, Photos, Notes
        sidebars) — an element inside the candidate's frame (± 2 pt) whose own title is empty or
        part of that label."""
        title = _norm(el.get("title") or "")
        label = _norm(c.label)
        if title and title == label:
            return True
        frame = el.get("frame")
        if not frame or not c.frame:
            return False
        fx, fy, fw, fh = frame
        x, y, w, h = c.frame
        inside = fx >= x - 2 and fy >= y - 2 and fx + fw <= x + w + 2 and fy + fh <= y + h + 2
        return inside and (not title or title in label or label in title)

    def click_candidate(self, c: Any) -> str:
        from .ax_candidates import is_sensitive

        cx, cy = self._centre(c)
        try:
            el = self.h._element_at(cx, cy)
        except Exception as e:  # noqa: BLE001 — a failed hit-test is a refusal, never a click
            return self._press_instead(c, f"refused: hit-test failed ({type(e).__name__})")
        if not el:
            return self._press_instead(c, f"refused: nothing under ({cx},{cy}) any more")
        title = str(el.get("title") or "")
        if el.get("app") and c.app and el["app"].casefold() != c.app.casefold():
            # Another app's window is over the point: the owner sees that window, so nothing is pressed under it.
            return f"refused: ({cx},{cy}) is in {el['app']}, not {c.app}"
        if not self._same_control(el, c):
            return self._press_instead(c, f"refused: under ({cx},{cy}) is {_what(el)}, not {c.label!r}")
        if (is_sensitive(title) or is_sensitive(c.label)) and self.approve not in (_norm(title), _norm(c.label)):
            return f"refused: {title or c.label!r} is a sensitive control without approval"
        self.h._last_click = _what(el)
        click = self.h.gui.click
        if not getattr(click, "_eve_wrapped", False):
            self.h._mark_acted()
        click(cx, cy)
        return f"clicked {_what(el)} at ({cx},{cy})"

    def _press_instead(self, c: Any, refusal: str) -> str:
        """The hit-test missed: AXPress the element the snapshot read for `c`, after checking it is still that
        control, or return `refusal` unchanged. No mouse input was sent before this, and none is sent here."""
        from .ax_candidates import is_sensitive

        snap = self._snapshot
        if not ax_press_on() or snap is None or snap.get(c.id) != c:
            return refusal                                   # off, or `c` is not from the snapshot just taken
        ref = snap.ref(c.id)
        if ref is None:
            return refusal
        if is_sensitive(c.label) and self.approve != _norm(c.label):
            return f"refused: {c.label!r} is a sensitive control without approval"
        try:
            live = self.h._element_read(ref)
        except Exception:  # noqa: BLE001 — an element that cannot be read is gone
            live = None
        if not live:
            return refusal
        frame = live.get("frame")
        title = _norm(live.get("title") or "")
        if (live.get("pid") != snap.pid or not live.get("enabled", True) or "AXPress" not in live.get("actions", ())
                or not frame or any(abs(a - b) > AX_PRESS_SLACK for a, b in zip(frame, c.frame, strict=True))
                or (title and title != _norm(c.label))):
            return refusal
        what = f"{live.get('role') or c.role} {c.label!r}"
        self.h._last_click = what
        self.h._mark_acted()
        try:
            err = int(self.h._element_press(ref))
        except Exception as e:  # noqa: BLE001
            err = -1
            log.warning("delegate: AXPress raised %s", type(e).__name__)
        if err != 0:
            # Sent, and the app answered an error: some apps do that after acting. The effect is unknown, so this
            # is not a refusal (that would let the lane try again); the lane's own check after it decides.
            return f"pressed {what} with AXPress, which answered error {err}; the effect is unknown"
        return f"pressed {what} with AXPress (the hit-test missed: {refusal[len('refused: '):]})"

    def focus_and_type(self, c: Any, text: str) -> str:
        line = self.click_candidate(c)
        if line.startswith("refused:"):
            return line
        self.h._sleep(0.1)
        fe = self.focused()
        if fe is None or not fe.editable or fe.frame == (0, 0, 0, 0):
            what = f"{fe.role} {fe.label!r}" if fe is not None else "nothing"
            return f"refused: focus is on {what}, not the {c.role} {c.label!r}"
        if any(abs(a - b) > 2 for a, b in zip(fe.frame, c.frame, strict=True)):
            return f"refused: focus moved to {fe.role} {fe.label!r} at {fe.frame}, not the {c.role} {c.label!r}"
        return self.h.type_text(text, submit=False)

    def press(self, key: str) -> str:
        press = self.h.gui.press
        if not getattr(press, "_eve_wrapped", False):
            self.h._mark_acted()
        press(str(key))
        return f"pressed {key}"

    def settle(self, secs: float) -> float:
        self.h._settle(float(secs))
        return float(self.h._elapsed)
