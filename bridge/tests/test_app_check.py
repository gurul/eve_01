"""app_check: every app Claude writes is used in a headless phone-sized browser before the owner sees it.

These run real headless Chromium (Playwright) against handwritten apps, a correct one and one for each way an
app breaks, so a finding is proven to fire on the bug and, as important, not to fire on a correct app: a false
alarm costs a repair round that can make a working app worse. No network: the fixtures load nothing remote.
"""

from __future__ import annotations

import asyncio
import time

import pytest

pytest.importorskip("playwright.async_api")

from eve_bridge.app_check import CheckReport, _value_for, check_app, data_shape  # noqa: E402

HEAD = """<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">
<title>{title}</title>
<style>
  body {{ margin: 0; padding: 16px; font: 16px -apple-system, system-ui, sans-serif;
         background: var(--tg-theme-bg-color, #fff); color: var(--tg-theme-text-color, #000); }}
  button, input {{ min-height: 44px; font: inherit; }}
  li {{ display: flex; gap: 8px; align-items: center; }}
  {style}
</style></head><body>"""

# A small, correct habit tracker: loads once, renders, saves after every change, confirms deletes natively.
GOOD = HEAD.format(title="Habits", style="") + """
<form id="f"><input id="name" placeholder="New habit" aria-label="Habit name" required>
<button type="submit">Add</button></form>
<ul id="list"></ul>
<p id="empty">No habits yet. Try "Drink water".</p>
<script>
const tg = window.Telegram.WebApp; tg.ready(); tg.expand();
const today = () => { const d = new Date(); return d.getFullYear() + "-" + String(d.getMonth() + 1).padStart(2, "0")
  + "-" + String(d.getDate()).padStart(2, "0"); };
let state = { v: 1, habits: [] };
async function persist() { try { await eve.save(state); } catch (e) { tg.showAlert("Could not save"); } }
function render() {
  const list = document.getElementById("list"); list.innerHTML = "";
  document.getElementById("empty").hidden = state.habits.length > 0;
  for (const h of state.habits) {
    const li = document.createElement("li");
    const done = h.days.includes(today());
    li.innerHTML = `<button class="tick" aria-label="Check in ${h.name}">${done ? "✓" : "○"}</button>
      <span>${h.name} · ${h.days.length} days</span><button class="del" aria-label="Delete ${h.name}">Delete</button>`;
    li.querySelector(".tick").onclick = () => { tg.HapticFeedback.impactOccurred("light");
      h.days = done ? h.days.filter((d) => d !== today()) : [...h.days, today()]; render(); persist(); };
    li.querySelector(".del").onclick = () => tg.showConfirm(`Delete ${h.name}?`, (ok) => {
      if (!ok) return; state.habits = state.habits.filter((x) => x !== h); render(); persist(); });
    list.appendChild(li);
  }
}
document.getElementById("f").addEventListener("submit", (e) => {
  e.preventDefault(); const name = document.getElementById("name").value.trim(); if (!name) return;
  state.habits.push({ id: String(Date.now()), name, days: [] }); document.getElementById("name").value = "";
  render(); persist();
});
(async () => { const saved = await eve.load(); if (saved && saved.v === 1) state = saved; render(); })();
</script></body></html>"""


def variant(old: str, new: str, base: str = GOOD) -> str:
    assert old in base, old
    return base.replace(old, new, 1)


THROWS_ON_TAP = variant("h.days = done ?", "undefinedHelper(h); h.days = done ?")
# Saves a shape its own loader cannot read back: fine on day one, broken on day two.
THROWS_ON_REOPEN = variant("if (saved && saved.v === 1) state = saved;",
                           "if (saved) { state = saved; state.habits.forEach((h) => h.streak.toFixed(0)); }")
NEVER_SAVES = variant("async function persist() { try { await eve.save(state); }",
                      "async function persist() { try { }")
FETCHES = variant("(async () => {", "fetch('https://api.example.com/habits').catch(() => {});\n(async () => {")
BODY = GOOD[len(HEAD.format(title="Habits", style="")):]
TOO_WIDE = HEAD.format(title="Wide", style=".banner { width: 640px; height: 40px; }") + BODY.replace(
    "<form", '<div class="banner"></div><form', 1)
NO_VIEWPORT = GOOD.replace('<meta name="viewport" content="width=device-width, initial-scale=1, viewport-fit=cover">', "")
RELOADS = variant("e.preventDefault(); ", "")
# Adds only through Telegram's MainButton, the way the prompt suggests for the primary action.
MAIN_BUTTON = variant('<button type="submit">Add</button>', "").replace(
    "(async () => {", "tg.MainButton.setText('Add habit').show().onClick(() => "
    "document.getElementById('f').requestSubmit());\n(async () => {", 1)
FREEZES = variant("(async () => {", "document.addEventListener('click', () => { while (true) {} });\n(async () => {")


def run(html: str, **kw) -> CheckReport:
    return asyncio.run(check_app(html, **kw))


def test_a_correct_app_passes_quickly_and_comes_back_with_a_screenshot() -> None:
    t0 = time.perf_counter()
    r = run(GOOD)
    took = time.perf_counter() - t0
    assert r.ok, r.issues
    assert r.fields >= 1 and r.taps >= 2 and r.saves >= 2 and r.skipped == ""
    assert r.saved["v"] == 1                                      # used, then its delete confirmed and saved
    assert r.screenshot and r.screenshot[:2] == b"\xff\xd8"       # a JPEG
    assert took < 15, took
    assert r.summary().startswith("passed")


def test_the_reopen_and_the_screenshot_use_the_data_from_before_the_deletes() -> None:
    # delete runs last; the owner's screenshot shows the app with a habit in it, not the emptied one
    r = run(GOOD)
    assert r.saved["habits"] == []                                # the check deleted what it made…
    assert r.ok and len(r.screenshot) > 10_000                    # …but photographed the app with data


def test_an_error_on_tap_names_the_tap_and_the_line() -> None:
    r = run(THROWS_ON_TAP)
    assert not r.ok
    hit = [i for i in r.issues if "undefinedHelper" in i]
    assert hit and "tapping" in hit[0] and "Check in" in hit[0] and "(line " in hit[0]


def test_an_app_that_cannot_read_its_own_saved_data_is_caught_on_reopen() -> None:
    r = run(THROWS_ON_REOPEN)
    assert any("reopening the app" in i and "toFixed" in i for i in r.issues), r.issues


def test_an_app_that_never_saves_is_caught() -> None:
    r = run(NEVER_SAVES)
    assert r.saves == 0 and any("never called eve.save" in i for i in r.issues), r.issues


def test_an_app_used_only_by_taps_that_never_saves_is_caught() -> None:
    taps_only = (HEAD.format(title="Counter", style="") + '<p id="n">0</p><button id="b">Add one</button><script>'
                 "let n = 0; eve.load().then((d) => { n = (d && d.n) || 0; document.getElementById('n').textContent = n; });"
                 "document.getElementById('b').onclick = () => { n++; document.getElementById('n').textContent = n; };"
                 "</script></body></html>")
    r = run(taps_only)
    assert any("never called eve.save" in i for i in r.issues), r.issues
    fixed = run(taps_only.replace("textContent = n; };", "textContent = n; eve.save({ n }); };"))
    assert fixed.ok, fixed.issues                     # the same app, saving: passes


def test_reaching_another_host_is_caught_and_blocked() -> None:
    r = run(FETCHES)
    assert any("api.example.com" in i for i in r.issues), r.issues


def test_a_page_wider_than_the_phone_is_caught() -> None:
    r = run(TOO_WIDE)
    assert any("wider than a 390px phone" in i and "sideways" in i for i in r.issues), r.issues
    assert not any("desktop" in i for i in r.issues)


def test_a_page_without_a_viewport_meta_is_caught() -> None:
    r = run(NO_VIEWPORT)
    assert any("desktop" in i and "name=\"viewport\"" in i for i in r.issues), r.issues


def test_a_form_that_reloads_the_page_is_caught() -> None:
    r = run(RELOADS)
    assert any("reloaded" in i and "preventDefault" in i for i in r.issues), r.issues


def test_the_main_button_is_pressed_like_a_person_would() -> None:
    r = run(MAIN_BUTTON)
    assert r.ok, r.issues
    assert r.saves >= 1


def test_an_app_that_freezes_is_reported_not_hung(monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge import app_check
    monkeypatch.setattr(app_check, "CHECK_TIMEOUT_S", 8.0)
    t0 = time.perf_counter()
    r = run(FREEZES)
    assert time.perf_counter() - t0 < 20
    assert any("stopped responding" in i for i in r.issues), r.issues


def test_a_seed_is_what_the_app_opens_with_and_is_never_changed() -> None:
    seed = {"v": 1, "habits": [{"id": "1", "name": "Stretch", "days": []}]}
    r = run(THROWS_ON_REOPEN, seed=seed)          # the owner's real data, shape without "streak": caught at once
    assert any("opening the app" in i and "toFixed" in i for i in r.issues), r.issues
    assert seed == {"v": 1, "habits": [{"id": "1", "name": "Stretch", "days": []}]}


def test_values_typed_into_fields_fit_the_field() -> None:
    import datetime as dt
    d = dt.date(2026, 9, 24)

    def field(tag="input", type_="", label="", **kw):
        return {"tag": tag, "type": type_, "label": label, "named": label, **kw}

    assert _value_for(field(type_="date", label="Target date"), d) == "2026-09-24"
    assert _value_for(field(type_="number"), d) == "12"
    assert _value_for(field(label="Weight (kg)"), d) == "60"
    assert _value_for(field(type_="search", label="Search"), d) == ""
    assert _value_for(field(tag="textarea", label="Notes"), d) == "Felt good today."
    assert _value_for(field(label="Habit name"), d) == "Read 10 pages"
    assert _value_for(field(label="Title"), d) == "Morning run"
    assert _value_for(field(label="Author"), d) == "Ursula K. Le Guin"
    assert _value_for(field(type_="week"), d) == "2026-W39"
    # the placeholder's own example beats any guess, and a word in it is not mistaken for a number field
    assert _value_for(field(label="Name", placeholder="e.g. Stretch for 5 minutes"), d) == "Stretch for 5 minutes"
    assert _value_for(field(placeholder="For example: “Milk”, “Eggs”…"), d) == "Milk"
    assert _value_for(field(label="Name", placeholder="Stretch for 5 minutes"), d) == "Morning run"
    # numbers: what a prefilled field holds, inside min/max, on the step; decimals only where allowed
    assert _value_for(field(type_="number", label="Glass size (ml)", value="250"), d) == "250"
    assert _value_for(field(type_="number", label="Glass size (ml)"), d) == "250"
    assert _value_for(field(type_="number", label="Goal", min="1", max="5"), d) == "3"
    assert _value_for(field(type_="number", label="Amount"), d) == "24"
    assert _value_for(field(inputmode="decimal", label="Amount"), d) == "24.5"
    assert _value_for(field(type_="number", label="Weight", step="2.5", min="0"), d) == "60"
    assert _value_for(field(type_="number", label="Rating", min="1", max="5"), d) == "4"
    assert _value_for(field(inputmode="decimal", label="Monthly limit"), d) == "400"
    # a second try after the app rejected the first: a different value inside the range
    assert _value_for(field(type_="number", label="Reps", min="1", value="8"), d, alt=True) == "2"
    # a prefilled text field keeps what the app put there
    assert _value_for(field(label="Habit name", value="Drink water"), d) == "Drink water"


def test_what_the_check_made_is_reported() -> None:
    assert data_shape({"v": 1, "habits": [{"name": "a", "done": {"2026-09-24": 1}}, {"name": "b", "done": {}}]}) \
        == "habits 2, habits.done 1"
    assert data_shape({"v": 1, "items": []}) == "" and data_shape(None) == ""
    assert CheckReport(photo_data={"v": 1, "items": [1, 2]}).made() == "items 2"
    assert CheckReport().made() == "no records" and CheckReport().summary().endswith("0 saves")


def test_no_playwright_is_a_skip_not_a_failure(monkeypatch: pytest.MonkeyPatch) -> None:
    import builtins

    real = builtins.__import__

    def fake(name, *a, **k):
        if name.startswith("playwright"):
            raise ImportError(name)
        return real(name, *a, **k)

    monkeypatch.setattr(builtins, "__import__", fake)
    r = run(GOOD)
    assert r.skipped and r.summary().startswith("not tested") and r.issues == []


# ---- what a person sees: the dark theme, fingers, charts -----------------------------------------------------

def page(body: str, style: str = "", title: str = "Look") -> str:
    """A small app that loads and saves like a good one, with ``body`` and ``style`` to test how it looks."""
    return (HEAD.format(title=title, style=style) + body + "<script>eve.load().then(() => {});"
            "document.querySelectorAll('button').forEach((b) => b.addEventListener('click', () => eve.save({ v: 1 })));"
            "</script></body></html>")


SEG = ('<h1>Range</h1><div class="seg"><button aria-selected="true">Today</button><button aria-selected="false">Week</button></div>'
       "<p>Pick a range.</p>")
SEG_STYLE = (".seg { display: flex; padding: 2px; background: rgba(118, 118, 128, .16); border-radius: 10px; }"
             ".seg button { flex: 1; border: 0; background: none; color: var(--tg-theme-text-color); }")


def test_text_that_vanishes_in_the_dark_theme_is_caught() -> None:
    r = run(page("<h2 class='t'>Your streaks</h2>", ".t { color: #222; }"))
    assert any("dark theme" in i and "Your streaks" in i for i in r.issues), r.issues
    fine = run(page("<h2 class='t'>Your streaks</h2>", ".t { color: var(--tg-theme-text-color, #222); }"))
    assert fine.ok, fine.issues


def test_a_selected_segment_that_looks_like_the_others_in_the_dark_is_caught() -> None:
    faint = SEG_STYLE + ".seg button[aria-selected=true] { background: var(--tg-theme-section-bg-color); font-weight: 600;" \
        " box-shadow: 0 1px 3px rgba(0,0,0,.15); }"
    r = run(page(SEG, faint))
    assert any("selected" in i and '"Today"' in i and '"Week"' in i for i in r.issues), r.issues
    clear = SEG_STYLE + ".seg button[aria-selected=true] { background: var(--tg-theme-button-color);" \
        " color: var(--tg-theme-button-text-color); }"
    assert run(page(SEG, clear)).ok
    # a state shown inside the element (a filled dot) is a real difference, not a missing one
    dots = ('<h1>Days</h1><div class="seg"><button aria-pressed="true"><span class="d on"></span>Mon</button>'
            '<button aria-pressed="false"><span class="d"></span>Tue</button></div>')
    assert run(page(dots, SEG_STYLE + ".d { display: inline-block; width: 8px; height: 8px; } .d.on { background: red; }")).ok


def test_a_date_field_left_in_light_colors_in_the_dark_is_caught() -> None:
    field = '<label for="d">Date</label><input id="d" type="date">'
    dark = "input { background: var(--tg-theme-section-bg-color); color: var(--tg-theme-text-color); }"
    r = run(page(field, dark))
    assert any("color-scheme" in i and '"Date"' in i for i in r.issues), r.issues
    assert run(page(field, dark + ":root { color-scheme: light dark; }")).ok


def test_tap_targets_too_small_for_a_finger_are_caught_unless_their_hit_area_is_wider() -> None:
    tiny = '<button class="x" aria-label="Remove">×</button>'
    small = ".x { min-height: 0; width: 22px; height: 22px; padding: 0; position: relative; }"
    r = run(page(tiny, small))
    assert any("Tap targets too small" in i and '"Remove" 22x22px' in i for i in r.issues), r.issues
    wider = small + ".x::after { content: ''; position: absolute; inset: -12px; }"
    assert run(page(tiny, wider)).ok


def test_inputs_small_enough_to_make_iphone_zoom_are_caught() -> None:
    r = run(page('<input aria-label="Habit name" class="s">', ".s { font-size: 13px; }"))
    assert any("under 16px" in i and "Habit name" in i for i in r.issues), r.issues


def test_a_chart_bar_stretched_by_a_css_class_is_caught() -> None:
    chart = ('<svg viewBox="0 0 100 50" width="100%"><rect class="bar today" x="10" y="10" width="10" height="40"/>'
             '<rect class="bar" x="30" y="20" width="10" height="30"/></svg><section class="today">Today</section>')
    r = run(page(chart, ".today { display: block; width: 100%; }"))
    assert any("chart shape" in i and 'class="bar today"' in i for i in r.issues), r.issues
    assert run(page(chart.replace('class="today"', 'class="today-card"'), ".today-card { width: 100%; }")).ok


# ---- how it keeps data, and time -----------------------------------------------------------------------------

def test_a_save_that_only_lands_after_telegram_closed_the_app_is_caught() -> None:
    debounced = (HEAD.format(title="Counter", style="") + '<p id="n">0</p><button id="b">Add one</button><script>'
                 "let n = 0, t; eve.load().then((d) => { n = (d && d.n) || 0; document.getElementById('n').textContent = n; });"
                 "document.getElementById('b').onclick = () => { n++; document.getElementById('n').textContent = n;"
                 " clearTimeout(t); t = setTimeout(() => eve.save({ n }), 800); };</script></body></html>")
    r = run(debounced)
    assert any("only after it was closed" in i for i in r.issues), r.issues
    flushed = debounced.replace("</script>", "document.addEventListener('visibilitychange', () => {"
                                "if (document.visibilityState === 'hidden') { clearTimeout(t); eve.save({ n }); } });</script>")
    fixed = run(flushed)
    assert fixed.ok, fixed.issues


def test_an_hour_passes_so_a_running_timer_finishes_during_the_check() -> None:
    timer = variant("(async () => {", "setTimeout(() => finishFocusSession(), 25 * 60 * 1000);\n(async () => {")
    r = run(timer)
    assert any("finishFocusSession" in i and "for an hour" in i for i in r.issues), r.issues


def test_a_press_that_shows_an_error_is_tried_again_once_the_form_is_complete() -> None:
    # "Save" wants a size picked; the check presses it first (a form's primary action), reads the error,
    # picks a size, and presses again: the core flow runs, and the photo shows the record it made
    form = (HEAD.format(title="Coffee", style="") + '<h1>New order</h1><input id="name" aria-label="Order name">'
            '<button class="size">Small</button><button class="size">Large</button><p class="err" id="err"></p>'
            '<ul id="list"></ul><script>'
            "const tg = window.Telegram.WebApp; let size = '', state = { v: 1, orders: [] };"
            "document.querySelectorAll('.size').forEach((b) => b.onclick = () => { size = b.textContent; });"
            "tg.MainButton.setText('Save order').show().onClick(() => {"
            "  const name = document.getElementById('name').value.trim();"
            "  if (!name || !size) { document.getElementById('err').textContent = 'Pick a size first.'; return; }"
            "  document.getElementById('err').textContent = '';"
            "  state.orders.push({ name, size }); eve.save(state);"
            "  document.getElementById('list').innerHTML = state.orders.map((o) => '<li>' + o.size + '</li>').join(''); });"
            "eve.load().then((d) => { if (d) state = d; });</script></body></html>")
    r = run(form)
    assert r.ok, r.issues
    assert r.photo_data["orders"] and r.photo_data["orders"][0]["size"] in ("Small", "Large"), r.photo_data
    assert r.made().startswith("orders 1")


def test_a_field_is_labelled_by_what_a_person_reads_next_to_it() -> None:
    labelled = variant('<input id="name" placeholder="New habit" aria-label="Habit name" required>',
                       '<label for="name">What to track</label><input id="name" placeholder="e.g. Floss" required>')
    r = run(labelled)
    assert r.ok, r.issues
    assert 'typing into the field "What to track"' in r.trace
    assert any(h["name"] == "Floss" for h in r.photo_data["habits"]), r.photo_data


def test_a_toast_over_a_control_is_waited_out_but_a_bar_that_stays_is_reported() -> None:
    body = ('<button id="go" style="position:fixed;bottom:8px;left:16px;width:120px">Add item</button>'
            '<div class="toast">Saved</div>')
    toast = ".toast { position: fixed; bottom: 0; left: 0; width: 200px; height: 70px; background: #333; color: #fff; }"
    stays = run(page(body, toast))
    assert any("cannot be tapped" in i and '"Saved"' in i for i in stays.issues), stays.issues
    goes = run(page(body + "<script>setTimeout(() => document.querySelector('.toast').remove(), 1500);</script>", toast))
    assert goes.ok and goes.taps >= 1, goes.issues


def test_the_photo_puts_telegrams_bottom_bar_under_the_page_not_over_it() -> None:
    PIL = pytest.importorskip("PIL.Image")
    import io
    shot = run(MAIN_BUTTON.replace("</body>", '<div style="position:fixed;left:0;right:0;bottom:0;height:30px;'
                                              'background:#00ff00"></div></body>')).screenshot
    img = PIL.open(io.BytesIO(shot)).convert("RGB")
    assert img.size == (780, 1688)
    r, g, b = img.getpixel((390, (844 - 100 - 15) * 2))           # the page's own bottom edge, above the bar
    assert g > 200 and r < 80 and b < 80, (r, g, b)
    r, g, b = img.getpixel((30 * 2, (844 - 100 + 8 + 25) * 2))    # the MainButton, beside its label
    assert b > 180 and r < 90, (r, g, b)


def test_the_stand_in_is_telegrams_normal_mode_with_nothing_inset_at_the_top() -> None:
    from eve_bridge.app_check import STUBS_JS
    assert "const safe = { top: 0," in STUBS_JS and "isFullscreen: false" in STUBS_JS


# ---- review fixes: a crash is a finding, nothing leaves by WebSocket, storage throws as when served ----------

def test_a_page_the_app_crashes_is_a_finding_and_keeps_what_was_found(monkeypatch: pytest.MonkeyPatch) -> None:
    from eve_bridge import app_check

    async def crashing_use(page, sess, report, today, theme):
        sess.note("Console error while tapping \"Add\": boom")        # found before the tab died
        sess.step('tapping "Stress test"')
        cdp = await page.context.new_cdp_session(page)
        try:
            await asyncio.wait_for(cdp.send("Page.crash"), 3)          # what a runaway allocation ends in
        except Exception:  # noqa: BLE001 — the crashed tab never answers
            pass
        await page.evaluate("1")

    monkeypatch.setattr(app_check, "_use", crashing_use)
    r = run(GOOD)
    assert r.skipped == "" and not r.ok
    assert r.issues[0] == 'Console error while tapping "Add": boom'
    assert any(i.startswith('The app crashed the page while tapping "Stress test"') for i in r.issues), r.issues
    assert r.summary().startswith("2 issue(s)")


def test_a_checker_that_cannot_start_is_still_a_skip(monkeypatch: pytest.MonkeyPatch) -> None:
    class NoBrowser:
        async def new_context(self, **kw):
            raise RuntimeError("Browser has been closed")

    r = asyncio.run(check_app(GOOD, browser=NoBrowser()))
    assert r.skipped and r.issues == []


def test_a_websocket_never_leaves_the_check_and_is_reported() -> None:
    import socket
    import threading

    hits: list[bytes] = []
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen(5)
    port = srv.getsockname()[1]

    def accept() -> None:
        while True:
            try:
                c, _ = srv.accept()
            except OSError:
                return
            hits.append(c.recv(200))
            c.close()

    threading.Thread(target=accept, daemon=True).start()
    leaky = variant("(async () => {", f"new WebSocket('ws://127.0.0.1:{port}/x?d=' + 'secret');\n(async () => {{")
    try:
        r = run(leaky)
    finally:
        srv.close()
    assert hits == []                                             # nothing reached the listener
    assert any("WebSocket" in i and "127.0.0.1" in i for i in r.issues), r.issues


def test_browser_storage_throws_as_it_does_in_the_served_app() -> None:
    # The server serves apps sandboxed (an opaque origin), so an app leaning on browser storage breaks there.
    stores = variant("(async () => {", "window['local' + 'Storage'].setItem('x', '1');\n(async () => {")
    r = run(stores)
    assert any("sandboxed" in i and "localStorage" in i for i in r.issues), r.issues


# A header like the maker's: the title, and an icon button at its right edge, where eve's Change pencil sits.
CORNER = HEAD.format(title="Corner", style="header { display: flex; align-items: center; } header h1 { flex: 1; } "
                     ".icon { width: 44px; height: 44px; }") + BODY.replace(
    "<form", '<header><h1>Habits</h1><button class="icon" aria-label="Settings">⚙</button></header><form', 1)
CORNER_FREE = CORNER.replace("header { display: flex;", "header { padding-right: 56px; display: flex;")


def test_a_control_under_eves_change_pencil_is_a_finding_and_a_free_corner_is_not() -> None:
    from eve_bridge import app_check

    r = run(CORNER)
    corner = [i for i in r.issues if "Change pencil" in i]
    assert len(corner) == 1 and '"Settings"' in corner[0] and "padding-right: 56px" in corner[0], r.issues
    assert not any("Change pencil" in i for i in run(CORNER_FREE).issues)            # the fix the finding asks for
    assert app_check.PENCIL == {"top": 6, "right": 6, "size": 44}


def test_the_pencils_place_in_the_check_is_where_eve_js_draws_it() -> None:
    from eve_bridge import app_check
    from eve_bridge.apps_maker import EVE_JS

    p = app_check.PENCIL
    assert f"width: {p['size']}px !important; height: {p['size']}px !important" in EVE_JS
    assert f"top: calc({p['top']}px + var(--tg-safe-area-inset-top" in EVE_JS
    assert f"right: calc({p['right']}px + var(--tg-safe-area-inset-right" in EVE_JS
