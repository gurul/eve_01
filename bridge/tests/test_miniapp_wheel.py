"""The Lights card's colour wheel (iro.js) in headless Chromium: where each colour sits, one request in flight
while a drag goes on, the colour let go on sent last, errors, the chosen light, keys, touch, small phones, and a
eve that does not serve iro.js. The page runs against stubbed /api routes; nothing reaches a real light."""

from __future__ import annotations

import colorsys
import json
import math
import time
from pathlib import Path
from typing import Any

import pytest

pw = pytest.importorskip("playwright.sync_api")

SRC = Path(__file__).resolve().parents[1] / "src" / "eve_bridge"
PAGE = (SRC / "miniapp_page.html").read_text()
IRO = (SRC / "miniapp_iro.min.js").read_text()
TG = """window.Telegram={WebApp:new Proxy({initData:'x',initDataUnsafe:{},themeParams:{},version:'8.0',
 platform:'ios',colorScheme:'light',HapticFeedback:{impactOccurred(){},notificationOccurred(){},selectionChanged(){}}},
 {get(t,k){return k in t?t[k]:(()=>{})}})};"""
# wraps fetch before the page runs: a set can be delayed (a Bluetooth light is slow), and the page counts how many
# are in flight at once, so "one at a time" is measured in the browser, not by the stub
FETCH = """(() => {
  const real = window.fetch; window.__inflight = 0; window.__max = 0;
  window.fetch = async (u, o) => {
    const isSet = String(u).endsWith('api/lights') && o && String(o.body).includes('"set"');
    if (!isSet) return real(u, o);
    window.__inflight++; window.__max = Math.max(window.__max, window.__inflight);
    try { await new Promise((r) => setTimeout(r, %d)); return await real(u, o); }
    finally { window.__inflight--; }
  };
})();"""


def hsv(hexs: str) -> tuple[float, float]:
    r, g, b = (int(hexs[i:i + 2], 16) / 255 for i in (1, 3, 5))
    h, s, _ = colorsys.rgb_to_hsv(r, g, b)
    return h * 360, s


def near(h: float, want: float, tol: float = 12) -> bool:
    d = abs(h - want) % 360
    return min(d, 360 - d) <= tol


class Card:
    def __init__(self, p: Any, *, latency: float = 0, mode: str = "ok", iro: bool = True, width: int = 390,
                 touch: bool = False, scheme: str = "light") -> None:
        try:
            self.b = p.chromium.launch()
        except Exception as e:  # noqa: BLE001
            pytest.skip(f"Chromium does not start here ({type(e).__name__})")
        ctx = self.b.new_context(viewport={"width": width, "height": 900}, color_scheme=scheme, has_touch=touch,
                                 is_mobile=touch)
        self.pg = ctx.new_page()
        self.sets: list[dict] = []
        self.errors: list[str] = []
        self.latency = latency
        self.pg.on("pageerror", lambda e: self.errors.append(str(e)))
        self.pg.add_init_script(FETCH % int(latency * 1000))

        def route(r: Any) -> None:
            u = r.request.url
            if "telegram-web-app" in u:
                return r.fulfill(body=TG, content_type="text/javascript")
            if u.endswith("/iro.js"):
                return r.fulfill(body=IRO, content_type="application/javascript") if iro else r.fulfill(status=404)
            if u.endswith("/api/me"):
                return r.fulfill(body=json.dumps({"lights": [{"name": "lamp"}, {"name": "strip"}, {"name": "wiz"}]}),
                                 content_type="application/json")
            if u.endswith("/api/lights"):
                body = json.loads(r.request.post_data or "{}")
                if body.get("action") != "set":
                    return r.fulfill(body='{"ok": true, "lights": []}', content_type="application/json")
                self.sets.append(body)
                if mode == "500":
                    return r.fulfill(status=500, body="oops")
                if mode == "abort":
                    return r.abort()
                return r.fulfill(body=json.dumps({"ok": True, "line": f"Lights {body.get('color')}."}),
                                 content_type="application/json")
            if u.startswith("http://eve.test/"):
                return r.fulfill(body=PAGE, content_type="text/html")
            return r.fulfill(body="{}", content_type="application/json")
        self.pg.route("**/*", route)
        self.pg.goto("http://eve.test/")
        self.pg.wait_for_selector("#lights-card:not([hidden])", timeout=5000)

    def open(self) -> tuple[float, float, float]:
        self.pg.click(".swatches button.custom")
        self.pg.wait_for_selector("#wheel-wrap:not([hidden])")
        b = self.pg.locator("#wheel").bounding_box()
        return b["x"] + b["width"] / 2, b["y"] + b["height"] / 2, b["width"] / 2 - 2

    def settle(self, secs: float = 3.0) -> None:
        end, n, quiet = time.time() + secs, -1, time.time()
        while time.time() < end:
            self.pg.wait_for_timeout(50)
            if len(self.sets) != n:
                n, quiet = len(self.sets), time.time()
            elif time.time() - quiet > 0.3 + self.latency and self.pg.evaluate("window.__inflight") == 0:
                break
        self.pg.wait_for_timeout(100)

    def picked(self) -> str:
        return self.pg.inner_text("#wheel-picked")

    def said(self) -> str:
        return self.pg.inner_text("#lights-said")


@pytest.fixture(scope="module")
def p() -> Any:
    with pw.sync_playwright() as play:
        yield play


def test_each_colour_sits_where_the_docs_say(p: Any) -> None:
    c = Card(p)
    cx, cy, r = c.open()
    got = {}
    for name, (dx, dy) in {"top": (0, -0.85), "right": (0.85, 0), "bottom": (0, 0.85), "left": (-0.85, 0)}.items():
        c.pg.mouse.click(cx + dx * r, cy + dy * r)
        c.settle()
        got[name] = c.picked()
        assert c.sets[-1]["color"] == got[name] and hsv(got[name])[1] > 0.7
    assert near(hsv(got["top"])[0], 0) and near(hsv(got["right"])[0], 90) and near(hsv(got["bottom"])[0], 180)
    assert near(hsv(got["left"])[0], 270)
    c.pg.mouse.click(cx, cy)
    c.settle()
    assert hsv(c.picked())[1] < 0.08                      # the centre is white
    c.pg.mouse.move(cx, cy - r * 0.5)
    c.pg.mouse.down()
    c.pg.mouse.move(cx, cy - r * 3, steps=5)              # far outside the wheel: clamps to the rim
    c.pg.mouse.up()
    c.settle()
    assert hsv(c.picked())[1] > 0.95 and not c.errors
    c.b.close()


def test_a_slow_light_gets_one_request_at_a_time_and_the_last_colour_last(p: Any) -> None:
    c = Card(p, latency=0.4)
    cx, cy, r = c.open()
    c.pg.mouse.move(cx, cy - r * 0.9)
    c.pg.mouse.down()
    for i in range(60):                                   # once round the wheel
        a = i / 60 * 2 * math.pi
        c.pg.mouse.move(cx + r * 0.9 * math.sin(a), cy - r * 0.9 * math.cos(a))
        c.pg.wait_for_timeout(15)
    c.pg.mouse.move(cx, cy + r * 0.9)
    c.pg.mouse.up()
    c.settle(6)
    assert c.pg.evaluate("window.__max") == 1
    assert len(c.sets) < 10                               # 60 moves, a handful of requests
    assert c.sets[-1]["color"] == c.picked() and near(hsv(c.picked())[0], 180)
    assert c.picked() in c.said()
    c.b.close()


def test_the_in_flight_counter_sees_two_at_once(p: Any) -> None:
    """The control for the test above: two concurrent sets do count as 2, so 1 there means one at a time."""
    c = Card(p, latency=0.4)
    c.pg.evaluate("Promise.all([1, 2].map(() => fetch('api/lights', {method: 'POST', "
                  "body: JSON.stringify({action: 'set'})})))")
    assert c.pg.evaluate("window.__max") == 2
    c.b.close()


@pytest.mark.parametrize("mode,line", [("500", "Something went wrong."), ("abort", "eve didn't answer")])
def test_a_failed_request_says_so_and_the_wheel_still_works(p: Any, mode: str, line: str) -> None:
    c = Card(p, mode=mode)
    cx, cy, r = c.open()
    c.pg.mouse.click(cx, cy - r * 0.8)
    c.settle()
    assert line in c.said()
    n = len(c.sets)
    c.pg.mouse.click(cx + r * 0.8, cy)
    c.settle()
    assert len(c.sets) > n and not c.errors
    c.b.close()


def test_the_chosen_light_keys_and_reopening(p: Any) -> None:
    c = Card(p)
    cx, cy, r = c.open()
    c.pg.click("#light-targets button:has-text('Wiz')")
    c.pg.mouse.click(cx, cy - r * 0.8)
    c.settle()
    assert c.sets[-1]["target"] == "wiz"
    c.pg.focus("#wheel")
    h0 = hsv(c.picked())[0]
    for _ in range(3):
        c.pg.keyboard.press("ArrowRight")
    c.settle()
    assert near(hsv(c.picked())[0], (h0 + 30) % 360, 4) and c.sets[-1]["color"] == c.picked()
    for _ in range(12):
        c.pg.keyboard.press("ArrowDown")
    c.settle()
    assert hsv(c.picked())[1] < 0.02
    c.pg.click(".swatches button.custom")
    assert c.pg.is_hidden("#wheel-wrap")
    c.pg.click(".swatches button.custom")
    assert c.pg.is_visible("#wheel-wrap") and not c.errors
    c.b.close()


def test_a_touch_drag_on_a_phone_sends_and_does_not_scroll(p: Any) -> None:
    c = Card(p, touch=True, width=360)
    cx, cy, r = c.open()
    cdp = c.pg.context.new_cdp_session(c.pg)

    def touch(kind: str, x: float = 0, y: float = 0) -> None:
        cdp.send("Input.dispatchTouchEvent", {"type": kind, "touchPoints": [] if kind == "touchEnd" else
                                              [{"x": x, "y": y}]})
    touch("touchStart", cx, cy - r * 0.8)
    for i in range(1, 11):
        touch("touchMove", cx + r * 0.08 * i, cy - r * 0.8 + r * 0.16 * i)
        c.pg.wait_for_timeout(10)
    touch("touchEnd")
    c.settle()
    assert c.sets and c.sets[-1]["color"] == c.picked()
    assert c.pg.evaluate("window.scrollY") == 0
    assert c.pg.evaluate("document.documentElement.scrollWidth <= innerWidth") and not c.errors
    c.b.close()


def test_the_wheel_fits_a_small_phone(p: Any) -> None:
    c = Card(p, width=320)
    c.open()
    assert c.pg.locator("#wheel").bounding_box()["width"] <= 320 - 32
    assert c.pg.evaluate("document.documentElement.scrollWidth <= innerWidth")
    c.b.close()


def test_without_iro_the_card_keeps_its_swatches(p: Any) -> None:
    c = Card(p, iro=False)
    assert c.pg.locator(".swatches button.custom").count() == 0
    assert c.pg.locator(".swatches button").count() == 10
    c.pg.locator(".swatches button").first.click()
    c.pg.wait_for_timeout(300)
    assert c.sets and c.sets[-1]["color"] == "red" and not c.errors
    c.b.close()
