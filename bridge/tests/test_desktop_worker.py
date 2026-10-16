"""desktop_worker.py with a fake pyautogui: exec semantics, output shaping,
Retina normalisation, error and fail-safe handling, the line protocol, the
auto-screenshot rule with helpers, the observe operation and --release."""

from __future__ import annotations

import base64
import io
import json
from datetime import datetime
from types import SimpleNamespace

from PIL import Image

from eve_bridge import desktop_worker as dw
from eve_bridge.desktop_helpers import Frame, Helpers


class FailSafeException(Exception):
    pass


class FakeAutoGUI:
    FailSafeException = FailSafeException

    def __init__(self, points=(100, 50), pixels=(200, 100)) -> None:
        self._points = points
        self._pixels = pixels
        self.clicks: list[tuple[int, int]] = []
        self.hotkeys: list[tuple] = []
        self.presses: list[str] = []

    def size(self):
        return self._points

    def screenshot(self, **kwargs):
        return Image.new("RGB", self._pixels, (10, 20, 30))

    def click(self, x, y, clicks=1):
        self.clicks.append((x, y))

    def hotkey(self, *keys):
        self.hotkeys.append(tuple(keys))

    def press(self, key):
        self.presses.append(key)


def _ns(gui: FakeAutoGUI | None = None) -> dict:
    return {"__builtins__": __builtins__, "pyautogui": gui or FakeAutoGUI()}


def test_log_and_stdout_become_text_items() -> None:
    r = dw.execute("log('a', 1)\nprint('b')\nprint()", _ns())
    assert r == {"output": [{"type": "input_text", "text": "a 1"}, {"type": "input_text", "text": "b"}]}


def test_display_image_becomes_png_data_url() -> None:
    r = dw.execute("display(pyautogui.screenshot())", _ns())
    item = r["output"][0]
    assert item["type"] == "input_image" and item["detail"] == "original"
    png = base64.b64decode(item["image_url"].split(",", 1)[1])
    assert png.startswith(b"\x89PNG")
    assert Image.open(io.BytesIO(png)).size == (200, 100)     # not normalised without normalize_screenshots


def test_normalize_screenshots_resizes_to_points_and_crops_regions() -> None:
    gui = FakeAutoGUI(points=(100, 50), pixels=(200, 100))
    dw.normalize_screenshots(gui)
    assert gui.screenshot().size == (100, 50)
    assert gui.screenshot(region=(10, 10, 20, 5)).size == (20, 5)


def test_display_rejects_non_png() -> None:
    r = dw.execute("display(b'not a png')", _ns())
    assert "TypeError" in r["output"][0]["text"]


def test_globals_persist_between_calls() -> None:
    ns = _ns()
    dw.execute("x = 41", ns)
    assert dw.execute("log(x + 1)", ns)["output"] == [{"type": "input_text", "text": "42"}]


def test_exceptions_come_back_as_text_and_keep_partial_output() -> None:
    r = dw.execute("log('before')\n1/0", _ns())
    assert r["output"][0]["text"] == "before"
    assert "ZeroDivisionError" in r["output"][1]["text"]


def test_failsafe_is_a_terminal_error() -> None:
    r = dw.execute("raise pyautogui.FailSafeException()", _ns())
    assert r == {"error": {"code": "failsafe", "message": "desktop fail-safe: the mouse hit a screen corner"}}


def test_no_output_placeholder() -> None:
    assert dw.execute("pass", _ns())["output"] == [{"type": "input_text", "text": "exec_py completed with no output."}]


def test_text_limit() -> None:
    r = dw.execute("log('x' * 70000)", _ns())
    assert "size limit" in r["output"][-1]["text"]


def test_model_code_can_click() -> None:
    gui = FakeAutoGUI()
    dw.execute("pyautogui.click(3, 4)", _ns(gui))
    assert gui.clicks == [(3, 4)]


# ---- line protocol -----------------------------------------------------------------

def _serve(lines: list[str], ns: dict | None = None, monkeypatch=None) -> list[dict]:
    out: list[dict] = []
    if monkeypatch is not None:
        monkeypatch.setattr(dw, "emit", out.append)
    dw.serve(ns or _ns(), iter(ln + "\n" for ln in lines))
    return out


def test_serve_executes_and_echoes_ids(monkeypatch) -> None:
    out = _serve([json.dumps({"id": 7, "operation": "execute", "code": "log('hi')"})], monkeypatch=monkeypatch)
    assert out == [{"id": 7, "output": [{"type": "input_text", "text": "hi"}]}]


def test_serve_rejects_bad_requests(monkeypatch) -> None:
    out = _serve(["not json", json.dumps({"id": 1, "operation": "ping"}),
                  json.dumps({"id": 2, "operation": "execute", "code": "   "})], monkeypatch=monkeypatch)
    assert [o["error"]["code"] for o in out] == ["bad_json", "unsupported", "bad_code"]


def test_serve_after_failsafe_refuses_everything(monkeypatch) -> None:
    out = _serve([json.dumps({"id": 1, "operation": "execute", "code": "raise pyautogui.FailSafeException()"}),
                  json.dumps({"id": 2, "operation": "execute", "code": "log('again')"})], monkeypatch=monkeypatch)
    assert out[0]["error"]["code"] == "failsafe"
    assert out[1] == {"id": 2, "error": out[0]["error"]}


# ---- helpers: auto-screenshot, observe, --release --------------------------------------

def _helpers(gui: FakeAutoGUI) -> tuple[Helpers, dict, list[float]]:
    """A Helpers on fakes (static screen, Warp frontmost) installed into a namespace."""
    settled: list[float] = []
    frame = Frame.from_pil(Image.new("RGB", (200, 100), (10, 20, 30)))
    h = Helpers(gui, capture=lambda: frame, ocr=lambda f, level: [{"text": "Warp", "bx": 0.0, "by": 0.9,
                                                                   "bw": 0.1, "bh": 0.05, "conf": 0.9}],
                frontmost_fn=lambda: {"app": "Warp", "bundle": "dev.warp.Warp-Stable", "title": "zsh", "pid": 1},
                run=lambda *a, **k: None, clipboard=SimpleNamespace(copy=lambda t: None, paste=lambda: ""),
                clock=lambda: 0.0, sleep=lambda s: None,
                now=lambda: datetime(2026, 9, 6, 14, 2))
    original = h.settled_screenshot

    def spy(max_wait: float):
        settled.append(max_wait)
        return original(max_wait)
    h.settled_screenshot = spy      # type: ignore[method-assign]
    ns = _ns(gui)
    h.install(ns)
    return h, ns, settled


def _kinds(result: dict) -> list[str]:
    return [o["type"] for o in result["output"]]


def test_auto_screenshot_after_actions_errors_and_empty_calls() -> None:
    gui = FakeAutoGUI()
    h, ns, settled = _helpers(gui)
    r = dw.execute("pyautogui.click(1,2)", ns, h)
    # the call clicked, so the closing line is "[after your input]" (a call that only looked says "[after]")
    assert _kinds(r) == ["input_image", "input_text"] and r["output"][1]["text"].startswith(
        "[after your input] frontmost: Warp")
    assert settled == [1.5] and gui.clicks == [(1, 2)]
    r = dw.execute("x = screen_text()", ns, h)                       # text only: no image
    assert _kinds(r) == ["input_text", "input_text"] and r["output"][0]["text"] == "screen_text: 1 lines"
    assert settled == [1.5]
    r = dw.execute("pass", ns, h)                                     # empty: a picture, no wait
    assert _kinds(r) == ["input_image", "input_text"] and settled == [1.5, 0.0]
    r = dw.execute("display(pyautogui.screenshot())", ns, h)          # displayed itself: exactly one image
    assert _kinds(r) == ["input_image", "input_text"] and settled == [1.5, 0.0]
    r = dw.execute("1/0", ns, h)
    assert _kinds(r) == ["input_text", "input_image", "input_text"]
    assert "ZeroDivisionError" in r["output"][0]["text"] and r["output"][2]["text"].startswith("[after]")
    assert settled == [1.5, 0.0, 0.0]
    r = dw.execute("type_text('hi')", ns, h)                          # helper actions count too
    assert _kinds(r) == ["input_text", "input_image", "input_text"] and settled[-1] == 1.5
    assert gui.hotkeys == [("command", "v")]


def test_serve_observe_operation(monkeypatch) -> None:
    gui = FakeAutoGUI()
    h, ns, _ = _helpers(gui)
    out: list[dict] = []
    monkeypatch.setattr(dw, "emit", out.append)
    dw.serve(ns, iter([json.dumps({"id": 1, "operation": "observe"}) + "\n",
                       json.dumps({"id": 2, "operation": "ping"}) + "\n"]), h)
    assert out[0]["id"] == 1
    kinds = [o["type"] for o in out[0]["output"]]
    assert kinds.count("input_image") == 1
    assert out[0]["output"][1]["text"] == "frontmost: Warp — 'zsh'; screen 100x50; 14:02"
    assert out[1]["error"]["code"] == "unsupported" and "observe" in out[1]["error"]["message"]


def test_release_inputs_posts_key_and_mouse_ups() -> None:
    posted: list[tuple] = []
    quartz = SimpleNamespace(
        kCGHIDEventTap="hid", kCGEventLeftMouseUp="lup", kCGEventRightMouseUp="rup", kCGEventOtherMouseUp="oup",
        kCGMouseButtonLeft=0, kCGMouseButtonRight=1, kCGMouseButtonCenter=2,
        CGEventCreateKeyboardEvent=lambda src, code, down: ("key", code, down),
        CGEventCreate=lambda src: "ev", CGEventGetLocation=lambda ev: (3, 4),
        CGEventCreateMouseEvent=lambda src, kind, loc, button: ("mouse", kind, loc, button),
        CGEventPost=lambda tap, ev: posted.append((tap, ev)),
    )
    assert dw.release_inputs(quartz) == 131
    keys = [ev for tap, ev in posted if ev[0] == "key"]
    mice = [ev for tap, ev in posted if ev[0] == "mouse"]
    assert len(keys) == 128 and all(ev[2] is False for ev in keys) and [ev[1] for ev in keys] == list(range(128))
    assert mice == [("mouse", "lup", (3, 4), 0), ("mouse", "rup", (3, 4), 1), ("mouse", "oup", (3, 4), 2)]
    assert all(tap == "hid" for tap, _ in posted)


# ---- settle de-duplication and the timing dict (fast-lane integration) ------------------

class _Clock:
    def __init__(self) -> None:
        self.t = 0.0

    def now(self) -> float:
        return self.t

    def sleep(self, s: float) -> None:
        self.t = round(self.t + s, 6)


def _bench(frames: list, front_app: str = "Calendar", decider=None) -> tuple[Helpers, dict, _Clock, list[float]]:
    """Helpers on a fake clock that advances with sleep, cycling captures over `frames`,
    with `front_app` already frontmost; returns the timeouts of every real settle."""
    clock = _Clock()
    settles: list[float] = []
    n = [0]

    def capture() -> Frame:
        f = frames[n[0] % len(frames)]
        n[0] += 1
        return Frame.from_pil(f)

    gui = FakeAutoGUI()
    h = Helpers(gui, capture=capture, ocr=lambda f, level: [],
                frontmost_fn=lambda: {"app": front_app, "bundle": "", "title": "x", "pid": 1},
                run=lambda *a, **k: SimpleNamespace(returncode=0), clock=clock.now, sleep=clock.sleep,
                now=lambda: datetime(2026, 9, 21, 2, 0), menu_bar_points=0, decider=decider)
    original = h._settle

    def spy(timeout: float, interval: float = 0.25) -> bool:
        settles.append(timeout)
        return original(timeout, interval)
    h._settle = spy      # type: ignore[method-assign]
    ns = _ns(gui)
    h.install(ns)
    return h, ns, clock, settles


def _still() -> Image.Image:
    return Image.new("RGB", (200, 100), (10, 20, 30))


def _moving() -> list[Image.Image]:
    a = _still()
    b = a.copy()
    b.paste((255, 255, 255), (0, 0, 100, 100))
    return [a, b]


def test_open_app_auto_screenshot_settles_once() -> None:
    h, ns, clock, settles = _bench([_still()])
    r = dw.execute("open_app('Calendar')", ns, h)
    assert _kinds(r) == ["input_text", "input_image", "input_text"]          # the reply still carries an image
    assert settles == [1.5]                                                 # open_app's own settle, nothing after
    assert clock.t < 0.3, clock.t                                           # one 0.25 s settle interval in total
    assert r["timing"]["settle"] > 0 and r["timing"]["exec"] >= 0


def test_raw_click_still_settles_up_to_1_5s() -> None:
    h, ns, clock, settles = _bench(_moving())
    r = dw.execute("pyautogui.click(1, 2)", ns, h)
    assert _kinds(r) == ["input_image", "input_text"]
    assert settles == [1.5] and 1.5 <= clock.t < 1.8, clock.t              # the screen never settled: full wait


def test_timed_out_settle_reuses_the_latest_frame_without_a_second_wait() -> None:
    h, ns, clock, settles = _bench(_moving())
    r = dw.execute("open_app('Calendar')", ns, h)
    assert _kinds(r) == ["input_text", "input_image", "input_text"]
    assert settles == [1.5] and clock.t == 1.5
    png = base64.b64decode(r["output"][1]["image_url"].split(",", 1)[1])
    assert Image.open(io.BytesIO(png)).getpixel((20, 20)) == (10, 20, 30)


def test_explicit_settle_timeout_is_false_and_does_not_wait_again_for_video() -> None:
    h, ns, clock, settles = _bench(_moving())
    r = dw.execute("pyautogui.click(1, 2); log(wait_settled())", ns, h)
    texts = [o["text"] for o in r["output"] if o["type"] == "input_text"]
    assert "False" in texts and "screen still changing after 5.0 s" in texts
    assert "input_image" in _kinds(r) and settles == [5.0] and clock.t == 5.0


def test_timed_out_settle_is_not_reused_after_a_later_input() -> None:
    h, ns, clock, settles = _bench(_moving())
    dw.execute("open_app('Calendar'); pyautogui.click(1, 2)", ns, h)
    assert settles == [1.5, 1.5] and clock.t == 3.0


def test_timed_out_settle_is_not_reused_when_the_frame_is_stale() -> None:
    from eve_bridge.desktop_helpers import SETTLE_REUSE_SECS

    h, ns, clock, settles = _bench(_moving())
    ns["pause"] = lambda: clock.sleep(SETTLE_REUSE_SECS)
    dw.execute("open_app('Calendar'); pause()", ns, h)
    assert settles == [1.5, 1.5]
    assert 3.0 + SETTLE_REUSE_SECS <= clock.t <= 3.25 + SETTLE_REUSE_SECS


def test_settle_is_not_reused_after_a_later_input() -> None:
    h, ns, clock, settles = _bench([_still()])
    dw.execute("open_app('Calendar'); pyautogui.click(1, 2)", ns, h)
    assert settles == [1.5, 1.5]                                            # the click after the settle needs its own


def test_reply_carries_timing_with_helpers() -> None:
    h, ns, _clock, _s = _bench([_still()])
    r = dw.execute("x = screen_text()", ns, h)
    t = r["timing"]
    assert set(t) >= {"capture", "ocr", "encode", "exec"} and all(isinstance(v, float) for v in t.values())
    assert t["encode"] == 0.0                                               # a text-only call encodes nothing
    r = dw.execute("pyautogui.click(1, 2)", ns, h)
    assert r["timing"]["encode"] > 0 and r["timing"]["act"] >= 0 and "settle" in r["timing"]
    assert "timing" not in dw.execute("log(1)", _ns())                     # no helpers: no timing


# ---- the fast lane's worker wiring ------------------------------------------------------------

def test_start_fast_lane_loads_in_a_thread_and_reports_status() -> None:
    h, ns, _c, _s = _bench([_still()])
    off = {"EVE_FAST_LANE": "0", "EVE_LANE_FIRST": "0"}
    assert dw.start_fast_lane(h, off) == "off (EVE_FAST_LANE)" and not h.fast_lane
    h.install(ns)
    assert "delegate" not in ns
    # model mode needs a decider the owner named; the keyword path (the default) needs none
    env = {"EVE_FAST_LANE": "1", "EVE_FAST_LANE_DECIDE": "model", "EVE_LANE_FIRST": "0"}
    assert dw.start_fast_lane(h, env) == "off (model mode needs EVE_DECIDER=jev)" and not h.fast_lane
    hk, nsk, _ck, _sk = _bench([_still()])
    keyword_env = {"EVE_FAST_LANE": "1", "EVE_LANE_FIRST": "0"}
    assert dw.start_fast_lane(hk, keyword_env) == "ready (keyword gate)" and hk.fast_lane and hk.decider is None
    assert hk.lane_decide == "keyword" and hk.lane_first_on is False
    # jev mode with no decider named: the step asker decides, so the lane is ready and nothing is loaded
    hj, _nsj, _cj, _sj = _bench([_still()])
    jev_env = {"EVE_FAST_LANE": "1", "EVE_FAST_LANE_DECIDE": "jev", "EVE_LANE_FIRST": "0"}
    assert dw.start_fast_lane(hj, jev_env) == "ready (jev step asker; no decider)" and hj.fast_lane
    assert hj.decider is None and hj.lane_decide == "jev"
    calls: list[str] = []

    def loader(style: str):
        calls.append(style)
        return SimpleNamespace(available=True, load_ms=400.0, warm_ms=1500.0, judge=lambda *a, **k: None)
    env = {"EVE_FAST_LANE": "1", "EVE_DECIDER": "jev", "EVE_FAST_LANE_STYLE": "hinted",
           "EVE_FAST_LANE_DECIDE": "model", "EVE_LANE_FIRST": "0"}
    status = dw.start_fast_lane(h, env, loader=loader, thread=False)
    assert status.startswith("ready (load 400 ms, warm 1500 ms, style hinted)") and h.fast_lane and h.decider is not None
    assert calls == ["hinted"]
    h.install(ns)
    assert callable(ns["delegate"])
    r = dw.execute("x = screen_text()", ns, h)
    assert r["fast_lane"].startswith("ready (") and "timing" in r
    # a failing loader is a status line, never an exception, and the reply says so

    def broken(style: str):
        raise RuntimeError("HTTP 503 from the decider")
    h2, ns2, _c2, _s2 = _bench([_still()])
    assert dw.start_fast_lane(h2, env, loader=broken, thread=False) == "failed: RuntimeError: HTTP 503 from the decider"
    h2.install(ns2)
    assert dw.execute("pass", ns2, h2)["fast_lane"] == "failed: RuntimeError: HTTP 503 from the decider"


def test_fast_lane_config_defaults_follow_the_eval_decision() -> None:
    from eve_bridge.fast_lane import DEFAULT_STYLE, FAST_LANE_DEFAULT
    assert dw.fast_lane_config({}) == (FAST_LANE_DEFAULT, DEFAULT_STYLE) and DEFAULT_STYLE == "jev"
    assert dw.fast_lane_config({"EVE_FAST_LANE": "yes", "EVE_FAST_LANE_STYLE": "turbo"}) == (True, DEFAULT_STYLE)
    assert dw.fast_lane_config({"EVE_FAST_LANE_STYLE": "compact"})[1] == "compact"


# ---- the lane_first operation (GATES.md G7) -------------------------------------------------


def _lane_bench(monkeypatch, snapshots: list):
    """A worker bench whose accessibility snapshots are scripted and whose hit-test finds the
    Week radio button under its own centre, so the real Helpers → adapter → router path runs."""
    from eve_bridge import ax_candidates

    h, ns, _clock, _s = _bench([_still()])
    served = iter(snapshots)
    last = [snapshots[-1]]

    def fake_snapshot(_pid=None, **_kw):
        last[0] = next(served, last[0])
        return last[0]

    monkeypatch.setattr(ax_candidates, "ax_snapshot", fake_snapshot)
    h._element_at = lambda x, y: {"role": "AXRadioButton", "title": "Week", "frame": (240, 100, 60, 30),
                                  "app": "Calendar"}
    # the scripted snapshots belong to pid 42: the lane refuses to click when another app is in front
    h._front = lambda: {"app": "Calendar", "bundle": "com.apple.iCal", "title": "x", "pid": 42}   # type: ignore[method-assign]
    h.install(ns)
    return h, ns


def test_lane_first_operation_replies_with_the_route(monkeypatch) -> None:
    from test_fast_lane import calendar

    h, ns = _lane_bench(monkeypatch, [calendar(), calendar(True)])
    assert dw.start_fast_lane(h, {"EVE_LANE_FIRST": "1"}) == "off (EVE_FAST_LANE); router on"
    assert h.lane_first_on is True and h.fast_lane is False            # the router does not need the helper
    out: list[dict] = []
    monkeypatch.setattr(dw, "emit", out.append)
    dw.serve(ns, iter([json.dumps({"id": 1, "operation": "lane_first", "goal": "switch to week view"}) + "\n",
                       json.dumps({"id": 2, "operation": "lane_first"}) + "\n",
                       json.dumps({"id": 3, "operation": "execute", "code": "log('still serving')"}) + "\n"]), h)
    route = out[0]["lane_first"]
    assert out[0]["id"] == 1 and route["status"] == "complete" and route["clicked"] == ["Week"], route
    assert route["sentence"] == "Done. I clicked Week." and route["line"].startswith("script: complete")
    assert any(line.startswith("delegate step 1: Week (radio button)") for line in route["log"])
    assert h.gui.clicks == [(270, 115)] and "ax" in out[0]["timing"]
    assert out[1] == {"id": 2, "lane_first": {"status": "unavailable", "reason": "lane_first needs a string goal"}}
    assert out[2]["output"][0] == {"type": "input_text", "text": "still serving"}     # never terminal


def test_lane_first_without_the_lane_says_unavailable(monkeypatch) -> None:
    from test_fast_lane import calendar

    h, ns = _lane_bench(monkeypatch, [calendar()])
    assert dw.start_fast_lane(h, {"EVE_LANE_FIRST": "0"}) == "off (EVE_FAST_LANE)" and h.lane_first_on is False
    reply = dw.lane_first({"goal": "switch to week view"}, h)
    assert reply == {"lane_first": {"status": "unavailable", "reason": "the router is off (EVE_LANE_FIRST)"}}
    assert h.gui.clicks == []
    assert dw.lane_first({"goal": "x"}, None)["lane_first"]["status"] == "unavailable"
    # a router that raises is "the planner does it", with zero input
    h.lane_first_on = True
    h.lane_first = lambda goal, dry_run=False: (_ for _ in ()).throw(RuntimeError("ax down"))   # type: ignore[method-assign]
    broken = dw.lane_first({"goal": "switch to week view"}, h)["lane_first"]
    assert broken["status"] == "none" and broken["reason"] == "error:RuntimeError"
    from eve_bridge.fast_lane import LANE_FIRST_DEFAULT

    assert dw.lane_modes({}) == ("keyword", LANE_FIRST_DEFAULT)          # the default is the eval's decision
    assert dw.lane_modes({"EVE_FAST_LANE_DECIDE": "model", "EVE_LANE_FIRST": "on"}) == ("model", True)
    assert dw.lane_modes({"EVE_FAST_LANE_DECIDE": "nonsense", "EVE_LANE_FIRST": "off"}) == ("keyword", False)


def test_unknown_operation_is_still_unsupported(monkeypatch) -> None:
    out = _serve([json.dumps({"id": 1, "operation": "lane_second", "goal": "x"})], monkeypatch=monkeypatch)
    assert out[0]["error"]["code"] == "unsupported" and "lane_first" in out[0]["error"]["message"]


def test_decider_backend_selects_jev_only_on_the_model_path(monkeypatch) -> None:
    from eve_bridge import jev

    loads: list[dict] = []

    def fake_load(*, env=None, style="jev", **_kw):
        loads.append({"style": style, "route": (env or {}).get("EVE_JEV_ROUTE")})
        return SimpleNamespace(available=True, load_ms=1.0, warm_ms=250.0)

    monkeypatch.setattr(jev, "load", fake_load)
    assert dw.decider_backend({}) == "" and dw.decider_backend({"EVE_DECIDER": "JEV"}) == "jev"
    assert dw.decider_backend({"EVE_DECIDER": "gpt"}) == ""
    base = {"EVE_FAST_LANE": "1", "EVE_DECIDER": "jev", "EVE_LANE_FIRST": "0",
            "EVE_JEV_ROUTE": "openrouter"}
    # keyword mode: the gate decides, so a hosted model is not loaded at all — nothing leaves the Mac
    h, _ns, _c, _s = _bench([_still()])
    assert dw.start_fast_lane(h, base, thread=False) == "ready (keyword gate)"
    assert loads == [] and h.decider is None
    # model mode: Jev loads through jev.load, in the style it scored best with
    h2, _ns2, _c2, _s2 = _bench([_still()])
    status = dw.start_fast_lane(h2, {**base, "EVE_FAST_LANE_DECIDE": "model"}, thread=False)
    assert status.startswith("ready (load 1 ms, warm 250 ms, style jev)") and h2.decider is not None
    assert loads == [{"style": "jev", "route": "openrouter"}]


# ---- the outline and run_plan operations (plan once, execute: plan_executor.py) -----------------------


def test_outline_and_run_plan_run_through_the_real_helpers(monkeypatch) -> None:
    from test_fast_lane import calendar

    h, ns = _lane_bench(monkeypatch, [calendar(), calendar(), calendar(True)])
    # No switch set: no hosted model is configured, and the executor falls back to the keyword gate alone.
    assert dw.start_jev_asker(h, {}) == "off" and h.step_asker is None
    plan = {"steps": [{"kind": "click", "target": "the week view", "label_hint": "Week"}], "final_say": "Week it is."}
    out: list[dict] = []
    monkeypatch.setattr(dw, "emit", out.append)
    dw.serve(ns, iter([json.dumps({"id": 1, "operation": "outline"}) + "\n",
                       json.dumps({"id": 2, "operation": "run_plan", "plan": plan, "request": "week view"}) + "\n",
                       json.dumps({"id": 3, "operation": "run_plan"}) + "\n",
                       json.dumps({"id": 4, "operation": "run_plan", "plan": {"steps": [{"kind": "drag"}]}}) + "\n",
                       json.dumps({"id": 5, "operation": "execute", "code": "log('still serving')"}) + "\n"]), h)
    shown = out[0]["outline"]
    assert shown["app"] == "Calendar" and "Week (radio button)" in shown["lines"]
    assert any(line.startswith("Delete Event") and "asks the human first" in line for line in shown["lines"])
    assert not any("September 2026" in line for line in shown["lines"])            # never the window title
    ran = out[1]["run_plan"]
    # No success check in the plan: it ran, and is partial rather than complete (test_plan_success_guard).
    assert ran["status"] == "partial" and ran["reason"] == "no_success_check" and h.gui.clicks == [(270, 115)], ran
    assert ran["ledger"][0]["effect"] == "confirmed" and ran["ledger"][0]["how"] == "keyword" and "ax" in out[1]["timing"]
    assert out[2]["run_plan"]["status"] == "unavailable"
    assert out[3]["run_plan"]["status"] == "unavailable" and "bad plan" in out[3]["run_plan"]["reason"]
    assert out[4]["output"][0] == {"type": "input_text", "text": "still serving"}     # never terminal


def test_the_jev_asker_is_configured_only_by_the_owners_switches() -> None:
    from eve_bridge.desktop_helpers import Helpers

    h = Helpers(FakeAutoGUI())
    assert dw.start_jev_asker(h, {"OPENROUTER_API_KEY": "k", "EVE_JEV_ROUTE": "openrouter"}) == "off"
    assert dw.start_jev_asker(h, {"EVE_PLAN_EXEC": "1"}).startswith("off (TYPESAFE_API_KEY is not set")
    assert h.step_asker is None and h._lane_decide() == "keyword"
    h.lane_decide = "jev"
    assert h._lane_decide() == "keyword"                     # jev asked for, none configured: the gate decides alone
    status = dw.start_jev_asker(h, {"EVE_PLAN_EXEC": "1", "OPENROUTER_API_KEY": "k",
                                    "EVE_JEV_ROUTE": "openrouter"})
    assert status == "ready (typesafe/jev-1.13)" and callable(h.step_asker) and h._lane_decide() == "jev"
    assert h._script_decide() == "keyword"                   # exact labels are the keyword gate's case
