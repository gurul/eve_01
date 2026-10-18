"""browser_lane.py: a page as a Snapshot, Playwright as the hands, the plan executor unchanged on top.

The live tests launch the real headless Chromium against a local HTML page (no network); they skip when
Playwright's browser is not installed. Everything else runs on fakes.
"""

from __future__ import annotations

import asyncio
import os
from pathlib import Path
from typing import Any

import pytest

from eve_bridge import browser_lane as bl
from eve_bridge.browser_lane import (
    BrowserLane,
    BrowserLaneConfig,
    is_web_goal,
    outline_lines,
    snapshot_from_page,
)

PAGE = """<!doctype html><html><head><title>Eve Shop</title></head><body>
<h1>Welcome</h1>
<nav><a href="#news">Top stories</a> <a href="#account">Your account</a></nav>
<form onsubmit="event.preventDefault(); document.getElementById('out').textContent='Results for '+document.getElementById('q').value; document.title='Results - Eve Shop';">
  <label for="q">Search products</label><input id="q" type="search" placeholder="Search">
  <button type="submit">Search</button>
</form>
<label><input type="checkbox" id="c1"> Email me deals</label>
<div role="tablist"><button role="tab" aria-selected="true">Week</button><button role="tab" aria-selected="false">Month</button></div>
<button onclick="document.getElementById('out').textContent='Order placed'">Place Order</button>
<input type="password" aria-label="Password">
<p id="out"></p>
<a href="#" style="display:none">Hidden link</a>
</body></html>"""


def _has_chromium() -> bool:
    try:
        from playwright.sync_api import sync_playwright
    except ImportError:
        return False
    try:
        with sync_playwright() as p:
            b = p.chromium.launch(headless=True)
            b.close()
        return True
    except Exception:  # noqa: BLE001
        return False


def live(test):
    """A real headless Chromium: opt-in with EVE_LIVE=1 (tests/conftest.py), and only then is Chromium
    probed — no browser launch at import on a plain run."""
    wanted = os.environ.get("EVE_LIVE", "").strip().lower() in ("1", "true", "yes", "on")
    probe = pytest.mark.skipif(wanted and not _has_chromium(), reason="Playwright's Chromium is not installed")
    return pytest.mark.live(probe(test))


@pytest.fixture
def page_url(tmp_path: Path) -> str:
    """The page over loopback HTTPS is not possible without a certificate, so the plan tests (whose
    contract insists on https) get a real page through a plain http server and a plan whose open_url
    is patched in below; the senses test uses file://."""
    f = tmp_path / "shop.html"
    f.write_text(PAGE)
    return f.as_uri()


@pytest.fixture
def served_url(tmp_path: Path):
    import functools
    import http.server
    import threading

    (tmp_path / "shop.html").write_text(PAGE)
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(tmp_path))
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    srv.RequestHandlerClass.log_message = lambda *a, **k: None
    t = threading.Thread(target=srv.serve_forever, daemon=True)
    t.start()
    yield f"http://127.0.0.1:{srv.server_address[1]}/shop.html"
    srv.shutdown()


def lane_for(tmp_path: Path, **kw: Any) -> BrowserLane:
    cfg = BrowserLaneConfig(profile=tmp_path / "profile", headless=True)
    return BrowserLane(cfg, **kw)


# ---- pure ------------------------------------------------------------------------------------

def test_web_goals_are_decided_by_code() -> None:
    for web in ("open google news and show me the top headline", "go to https://example.com", "search the web for ramen",
                "look up the weather", "open amazon", "on github open my pull requests", "in the browser open bbc"):
        assert is_web_goal(web), web
    for mac in ("open the calculator", "pause the music", "put the calendar on the year view", "open mail"):
        assert not is_web_goal(mac), mac
    assert is_web_goal("anything", route_kind="search") and is_web_goal("anything", route_kind="open_url")


def test_a_page_becomes_a_snapshot_the_gate_can_read() -> None:
    raw = {"title": "Eve Shop", "url": "file:///shop.html", "dialog": "", "elements": [
        {"id": "0", "role": "link", "name": "Top stories", "value": "", "x": 10, "y": 40, "w": 80, "h": 20, "enabled": True,
         "secure": False, "editable": False, "dialog": False, "focused": False},
        {"id": "1", "role": "searchbox", "name": "Search products", "value": "", "x": 10, "y": 80, "w": 200, "h": 30,
         "enabled": True, "secure": False, "editable": True, "dialog": False, "focused": True},
        {"id": "2", "role": "password", "name": "Password", "value": "", "x": 10, "y": 120, "w": 200, "h": 30,
         "enabled": True, "secure": True, "editable": False, "dialog": False, "focused": False},
        {"id": "3", "role": "tab", "name": "Week", "value": "selected", "x": 10, "y": 160, "w": 60, "h": 30,
         "enabled": True, "secure": False, "editable": False, "dialog": False, "focused": False}]}
    snap = snapshot_from_page(raw, seq=3, secs=0.01)
    assert snap.app == "browser" and snap.title == "Eve Shop" and snap.seq == 3 and snap.pid == 1
    assert [c.role for c in snap.elements] == ["link", "search field", "secure text field", "tab"]
    assert snap.focused is not None and snap.focused.label == "Search products" and snap.focused.editable
    assert snap.get("2").secure and snap.get("3").value == "selected"
    assert "url: file:///shop.html" in snap.context_lines
    lines = outline_lines(snap)
    assert lines == ["link: Top stories", "search field: Search products", "tab: Week, selected"]   # no secure field


# ---- live: the real browser through the real executor ---------------------------------------------

@live
def test_the_page_senses_and_effectors_keep_the_lane_contract(tmp_path: Path, page_url: str) -> None:
    lane = lane_for(tmp_path)

    async def go() -> None:
        assert (await lane.open_url(page_url)).startswith("opened ")
        outline = await lane.outline()
        assert outline["app"] == "browser" and outline["title"] == "Eve Shop"
        assert "button: Search" in outline["lines"] and "link: Top stories" in outline["lines"]
        assert "checkbox: Email me deals, unchecked" in outline["lines"] and "tab: Week, selected" in outline["lines"]
        assert not any("Password" in ln for ln in outline["lines"]) and not any("Hidden" in ln for ln in outline["lines"])

        def probe() -> dict[str, Any]:
            p = lane._ensure()
            snap = p.snapshot()
            box = next(c for c in snap.elements if c.label == "Email me deals")
            field = next(c for c in snap.elements if c.label == "Search products")
            order = next(c for c in snap.elements if c.label == "Place Order")
            pw = next(c for c in snap.elements if c.secure)
            out: dict[str, Any] = {}
            out["click"] = p.click_candidate(box)
            after = p.snapshot()
            out["box_after"] = next(c for c in after.elements if c.label == "Email me deals").value
            out["changed"] = p.screen_changed()
            out["sensitive"] = p.click_candidate(order)
            p.approve = "place order"
            out["approved"] = p.click_candidate(order)
            out["order_text"] = p.text_visible("Order placed")
            out["secure"] = p.focus_and_type(pw, "hunter2")
            out["typed"] = p.focus_and_type(field, "ramen")
            out["pressed"] = p.press("return")
            p.settle(0.5)
            out["results"] = p.text_visible("Results for ramen")
            out["title"] = p.snapshot().title
            out["focused"] = p.focused().label if p.focused() else None
            with pytest.raises(ValueError):
                p.press("f13")
            return out

        out = await lane._run(probe)
        assert out["click"].startswith("clicked checkbox 'Email me deals'") and out["box_after"] == "checked"
        assert out["changed"] is True
        assert out["sensitive"].startswith("refused:") and "sensitive" in out["sensitive"]     # code, not prompt
        assert out["approved"].startswith("clicked") and out["order_text"] is True
        assert out["secure"].startswith("refused:") and "secure" in out["secure"]
        assert out["typed"] == "typed 5 characters into search field 'Search products'"
        assert out["pressed"] == "pressed return" and out["results"] is True
        assert out["title"] == "Results - Eve Shop"
        await lane.close()

    asyncio.run(go())


@live
def test_a_plan_runs_through_the_real_executor_with_no_model(tmp_path: Path, served_url: str) -> None:
    lane = lane_for(tmp_path)
    page_url = served_url.replace("http://", "https://")            # the contract insists; the lane is told
    lane._plain_http = {page_url: served_url}
    plan = {"steps": [
        {"kind": "open_url", "target": page_url, "expect": {"kind": "title_contains", "value": "Eve Shop"}},
        {"kind": "click", "target": "the Email me deals checkbox", "label_hint": "Email me deals",
         "expect": {"kind": "text_visible", "value": "Email me deals"}},
        {"kind": "type", "target": "Search products", "text": "ramen", "expect": {"kind": "text_visible", "value": "ramen"}},
        {"kind": "press_key", "key": "return", "expect": {"kind": "text_visible", "value": "Results for ramen"}},
    ], "final_say": "Searched for ramen.", "success": {"kind": "title_contains", "value": "Results"}}
    goal = "on the shop site tick email me deals and search for ramen"

    async def go() -> dict[str, Any]:
        result = await lane.run_plan(plan, goal)
        await lane.close()
        return result

    result = asyncio.run(go())
    assert result["status"] == "complete", result
    assert result["sentence"] == "Searched for ramen."
    effects = [(e["step"], e["effect"], e["how"]) for e in result["ledger"]]
    assert effects[0][1] == "confirmed" and effects[0][2] == "helper"
    assert effects[1][1] == "confirmed" and effects[1][2] == "keyword"
    assert effects[3][1] == "confirmed"
    assert result["ms"] < 15000


@live
def test_a_sensitive_control_stops_the_plan_for_the_human_and_a_yes_lets_it_through(tmp_path: Path, served_url: str) -> None:
    lane = lane_for(tmp_path)
    page_url = served_url.replace("http://", "https://")
    lane._plain_http = {page_url: served_url}
    plan = {"steps": [{"kind": "open_url", "target": page_url},
                      {"kind": "click", "target": "the Place Order button", "label_hint": "Place Order",
                       "expect": {"kind": "text_visible", "value": "Order placed"}}],
            "final_say": "Ordered.", "success": None}

    async def go() -> tuple[dict[str, Any], dict[str, Any]]:
        first = await lane.run_plan(plan, "place the order on the shop site")
        second = await lane.run_plan(plan, "place the order on the shop site", start=first["next_index"],
                                     approved={str(first["next_index"]): first["confirm"]})
        await lane.close()
        return first, second

    first, second = asyncio.run(go())
    assert first["status"] == "needs_human" and first["confirm"].casefold() == "place order"
    assert not any(e["effect"] == "confirmed" and "Order" in e["step"] for e in first["ledger"])
    assert second["status"] == "complete" and second["sentence"] == "Ordered."


# ---- what the eval's fixture pages taught (tools/browser_model_eval.py, 2026-09-24) -----------------------

def _serve(tmp_path: Path, name: str, html: str):
    import functools
    import http.server
    import threading

    (tmp_path / name).write_text(html)
    handler = functools.partial(http.server.SimpleHTTPRequestHandler, directory=str(tmp_path))
    srv = http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler)
    srv.RequestHandlerClass.log_message = lambda *a, **k: None
    threading.Thread(target=srv.serve_forever, daemon=True).start()
    return srv, f"http://127.0.0.1:{srv.server_address[1]}/{name}"


BELOW_THE_FOLD = """<!doctype html><html><head><title>Pricing</title></head><body>
<nav><a href="#top">Home</a></nav><h1>Pricing</h1><div style="height:3000px">Features and more features.</div>
<button onclick="document.getElementById('out').textContent='Starter $5, Team $15'">Compare all plans</button>
<p id="out"></p></body></html>"""


@live
def test_a_control_below_the_fold_is_collected_and_scrolled_to_before_the_click(tmp_path: Path) -> None:
    srv, url = _serve(tmp_path, "pricing.html", BELOW_THE_FOLD)
    lane = lane_for(tmp_path)
    plan = {"steps": [{"kind": "click", "target": "the Compare all plans button", "label_hint": "Compare all plans",
                       "expect": {"kind": "text_visible", "value": "Starter $5"}}],
            "final_say": "Compared.", "success": None}

    async def go() -> tuple[dict[str, Any], dict[str, Any]]:
        await lane.open_url(url)
        outline = await lane.outline()
        result = await lane.run_plan(plan, "click the Compare all plans button at the bottom")
        await lane.close()
        return outline, result

    try:
        outline, result = asyncio.run(go())
    finally:
        srv.shutdown()
    assert "button: Compare all plans" in outline["lines"]              # the planner can see it
    assert result["status"] == "complete" and result["ledger"][0]["effect"] == "confirmed", result


def _recipes(buttons: str) -> str:
    return ("""<!doctype html><html><head><title>Recipes</title></head><body><h1>Recipes</h1>
<ul><li><a href="#apple" onclick="document.title='Apple Crumble'">Apple Crumble</a></li>
<li><a href="#lemon" onclick="document.title='Lemon Tart'">Lemon Tart</a></li></ul>
<div role="dialog" style="display:none">A hidden dialog is not an open one.</div>
<div id="cookie" role="dialog" aria-label="Cookie consent" style="position:fixed;inset:0;background:rgba(0,0,0,.5)">
<div style="background:#fff;padding:24px"><p>We use cookies to improve your experience.</p>""" + buttons
            + "</div></div></body></html>")


REJECT = ("<button onclick=\"document.getElementById('cookie').remove(); window.choice='reject'\">Reject non-essential cookies</button>"
          "<button onclick=\"document.getElementById('cookie').remove(); window.choice='accept'\">Accept all cookies</button>")
ACCEPT_ONLY = "<button onclick=\"document.getElementById('cookie').remove(); window.choice='accept'\">Accept all cookies</button>"


def test_a_consent_notice_is_dismissed_only_by_refusing_or_closing() -> None:
    names = lambda *xs: [{"id": str(i), "name": x} for i, x in enumerate(xs)]  # noqa: E731
    assert bl.consent_choice(names("Accept all cookies", "Reject non-essential cookies"))["name"].startswith("Reject")
    assert bl.consent_choice(names("Agree", "Necessary only"))["name"] == "Necessary only"
    assert bl.consent_choice(names("OK", "Close"))["name"] == "Close"
    for agreeing in (names("Accept all cookies"), names("I agree", "OK"), names("Accept necessary cookies"), names()):
        assert bl.consent_choice(agreeing) is None, agreeing
    assert bl.CONSENT_TEXT.search("We use cookies to improve your experience.")
    assert not bl.CONSENT_TEXT.search("Delete this event?")


@live
def test_a_cookie_notice_over_the_page_is_refused_before_the_plan_clicks(tmp_path: Path) -> None:
    srv, url = _serve(tmp_path, "recipes.html", _recipes(REJECT))
    lane = lane_for(tmp_path)
    plan = {"steps": [{"kind": "click", "target": "the Lemon Tart link", "label_hint": "Lemon Tart",
                       "expect": {"kind": "title_contains", "value": "Lemon Tart"}}],
            "final_say": "Opened.", "success": None}

    async def go() -> tuple[dict[str, Any], dict[str, Any], Any]:
        await lane.open_url(url)
        result = await lane.run_plan(plan, "open the Lemon Tart recipe")
        outline = await lane.outline()
        choice = await lane._run(lambda: lane._ensure().page.evaluate("() => window.choice"))
        await lane.close()
        return outline, result, choice

    try:
        outline, result, choice = asyncio.run(go())
    finally:
        srv.shutdown()
    assert result["status"] == "complete", result
    assert result["ledger"][0]["step"] == "dismiss the cookie notice" and choice == "reject"
    assert "link: Lemon Tart" in outline["lines"]


@live
def test_a_notice_that_only_offers_to_accept_stays_up_and_stops_the_plan(tmp_path: Path) -> None:
    srv, url = _serve(tmp_path, "recipes.html", _recipes(ACCEPT_ONLY))
    lane = lane_for(tmp_path)
    plan = {"steps": [{"kind": "click", "target": "the Lemon Tart link", "label_hint": "Lemon Tart"}],
            "final_say": "Opened.", "success": None}

    async def go() -> tuple[dict[str, Any], Any]:
        await lane.open_url(url)
        result = await lane.run_plan(plan, "open the Lemon Tart recipe")
        choice = await lane._run(lambda: lane._ensure().page.evaluate("() => window.choice || ''"))
        await lane.close()
        return result, choice

    try:
        result, choice = asyncio.run(go())
    finally:
        srv.shutdown()
    assert result["status"] == "none" and result["reason"] == "dialog_open" and choice == "", result


SEARCH_HOME = """<!doctype html><html><head><title>Docs</title></head><body><h1>Docs</h1>
<form action="results.html" method="get" role="search"><input type="search" name="q" aria-label="Search docs"></form>
</body></html>"""
RESULTS = """<!doctype html><html><head><title>Results</title></head><body><a href="#guide">Solar Panel Installation Guide</a></body></html>"""


@live
def test_return_in_a_search_field_waits_for_the_results_page(tmp_path: Path) -> None:
    (tmp_path / "results.html").write_text(RESULTS)
    srv, url = _serve(tmp_path, "docs.html", SEARCH_HOME)
    lane = lane_for(tmp_path)
    plan = {"steps": [{"kind": "type", "target": "the Search docs field", "label_hint": "Search docs", "text": "solar panels"},
                      {"kind": "press_key", "key": "return"}], "final_say": "Searched.", "success": None}

    async def go() -> tuple[dict[str, Any], str]:
        await lane.open_url(url)
        result = await lane.run_plan(plan, "search the docs for solar panels")
        seen = await lane._run(lambda: lane._ensure().page.url)       # read at once, no extra wait
        await lane.close()
        return result, seen

    try:
        result, seen = asyncio.run(go())
    finally:
        srv.shutdown()
    assert result["status"] == "complete" and "results.html?q=solar+panels" in seen, (result, seen)


ROUTE_APPS = ("Spotify", "Photo Booth", "Slack", "News", "Weather", "Maps", "Google Chrome", "Safari", "Notes")


def test_only_a_browser_only_goal_goes_to_the_chrome_lane() -> None:
    """The Chrome lane drives one tab: a web goal that also needs the Mac is Codex's (production, 2026-09-26:
    "Open Spotify on my computer and on my browser" went to Chrome)."""
    only = lambda g: bl.is_browser_only_goal(g, ROUTE_APPS)   # noqa: E731
    for goal in ("open up Google", "search the latest news on Google", "check my gmail", "look up the weather",
                 "open youtube and play lofi", "open github.com", "open spotify in chrome",
                 "google how to reset spotify", "look up how to use Photo Booth"):
        assert only(goal), goal
    for goal in ("Open Spotify on my computer and on my browser", "open spotify and chrome",
                 "Send the picture I just took on Photo Booth to me", "play my liked songs on Spotify",
                 "open the desktop app for youtube", "open Notes"):
        assert not only(goal), goal


# What was typed into a secret field must never reach a snapshot: an unlabelled password used its value as
# its name, and a card number (a plain text box) was copied into `value` for the planner and Jev to read.
SECRETS = {"pw": "hunter2-canary", "shown": "plain-pw-canary", "card": "4242424242424242",
           "cvc": "987", "otp": "135790", "search": "ramen-control"}
SECRET_FORM = """<!doctype html><html><head><title>Checkout</title></head><body><form>
<input id="pw" type="password">
<input id="shown" type="text" autocomplete="current-password">
<label for="card">Card number</label><input id="card" autocomplete="cc-number">
<label for="cvc">Security code</label><input id="cvc" autocomplete="billing cc-csc">
<label for="otp">Code</label><input id="otp" autocomplete="one-time-code">
<label for="search">Search</label><input id="search" type="search">
</form><script>const v = %s; for (const k in v) document.getElementById(k).value = v[k];</script></body></html>"""


@live
def test_what_is_typed_into_a_secret_field_never_reaches_the_snapshot(tmp_path: Path) -> None:
    import json
    srv, url = _serve(tmp_path, "checkout.html", SECRET_FORM % json.dumps(SECRETS))
    lane = lane_for(tmp_path)

    async def go() -> tuple[dict[str, Any], dict[str, Any]]:
        await lane.open_url(url)
        snap = await lane._run(lambda: lane._ensure().snapshot())
        outline = await lane.outline()
        await lane.close()
        return {c.label: c for c in snap.elements}, {"snap": [c.to_dict() for c in snap.elements], **outline}

    try:
        by_label, seen = asyncio.run(go())
    finally:
        srv.shutdown()
    dump = json.dumps(seen)
    for key, secret in SECRETS.items():
        if key != "search":
            assert secret not in dump, (key, dump)
    assert by_label["Search"].value == "ramen-control"            # an ordinary field is still read
    assert by_label["Card number"].value == "" and not by_label["Card number"].secure   # still typeable
    assert sum(c["secure"] for c in seen["snap"]) == 2, dump    # the password and the shown password


def test_an_attach_that_fails_after_connecting_closes_the_driver(tmp_path: Path,
                                                                 monkeypatch: pytest.MonkeyPatch) -> None:
    """An AttachError from _pick_profile after the CDP connection was up leaked the connection and the
    Playwright driver, and the next _ensure started a second driver. Fakes only: no real Chrome."""
    import sys
    from types import SimpleNamespace

    drivers: list[SimpleNamespace] = []

    def start() -> SimpleNamespace:
        browser = SimpleNamespace(contexts=[SimpleNamespace()])
        pw = SimpleNamespace(stopped=False, chromium=SimpleNamespace(connect_over_cdp=lambda *a, **k: browser))
        pw.stop = lambda: setattr(pw, "stopped", True)
        drivers.append(pw)
        return pw

    monkeypatch.setitem(sys.modules, "playwright.sync_api",
                        SimpleNamespace(sync_playwright=lambda: SimpleNamespace(start=start)))
    monkeypatch.setattr(bl, "devtools_endpoint", lambda *a: "ws://127.0.0.1:9/devtools/browser/x")
    monkeypatch.setattr(bl, "ensure_chrome_window", lambda: None)
    lane = BrowserLane(BrowserLaneConfig(profile=tmp_path / "p", attach=True, chrome_dir=tmp_path,
                                         chrome_profile="someone@example.com"))
    monkeypatch.setattr(lane, "_profiles", lambda: {})          # no window open in that profile
    for _ in range(2):
        with pytest.raises(bl.AttachError):
            lane._ensure()
        assert drivers[-1].stopped and lane._pw is None and lane._browser is None and lane._context is None
    assert len(drivers) == 2 and all(d.stopped for d in drivers)                # never two drivers alive at once
